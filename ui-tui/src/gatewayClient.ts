import { type ChildProcess, spawn } from 'node:child_process'
import { randomBytes } from 'node:crypto'
import { EventEmitter } from 'node:events'
import { existsSync } from 'node:fs'
import { delimiter, isAbsolute, join, resolve } from 'node:path'
import { createInterface } from 'node:readline'

import type { GatewayEvent } from './gatewayTypes.js'
import { CircularBuffer } from './lib/circularBuffer.js'

const MAX_GATEWAY_LOG_LINES = 200
const MAX_LOG_LINE_BYTES = 4096
const MAX_BUFFERED_EVENTS = 2000
const MAX_LOG_PREVIEW = 240
const STARTUP_TIMEOUT_MS = Math.max(5000, parseInt(process.env.HERMES_TUI_STARTUP_TIMEOUT_MS ?? '15000', 10) || 15000)
const REQUEST_TIMEOUT_MS = Math.max(30000, parseInt(process.env.HERMES_TUI_RPC_TIMEOUT_MS ?? '120000', 10) || 120000)
const WS_OPEN = 1
const WS_CLOSING = 2
const WS_CLOSED = 3

const truncateLine = (line: string) =>
  line.length > MAX_LOG_LINE_BYTES ? `${line.slice(0, MAX_LOG_LINE_BYTES)}… [truncated ${line.length} bytes]` : line

const resolveGatewayAttachUrl = () => {
  const raw = process.env.HERMES_TUI_GATEWAY_URL?.trim()

  return raw ? raw : null
}

const resolveSidecarUrl = () => {
  const raw = process.env.HERMES_TUI_SIDECAR_URL?.trim()

  return raw ? raw : null
}

// The Rust gateway (crates/hermes-desktop-gateway) reads its token from this variable and
// announces its port on stdout once it listens.
const GATEWAY_TOKEN_ENV = 'HERMES_DASHBOARD_SESSION_TOKEN'
const GATEWAY_READY_RE = /^HERMES_BACKEND_READY port=(\d+)$/
const GATEWAY_BIN = process.platform === 'win32' ? 'hermes-desktop-gateway.exe' : 'hermes-desktop-gateway'

const GATEWAY_BUILD_HINT =
  'build it with `cargo build --release -p hermes-desktop-gateway` or set HERMES_TUI_GATEWAY_BIN, then restart the TUI'

// On Windows `\tools` passes isAbsolute but names a folder on the current drive.
const isRootedPath = (p: string) =>
  process.platform === 'win32' ? /^([a-z]:[\\/]|[\\/]{2})/i.test(p) : isAbsolute(p)

// Only a rooted path, so spawn never searches the working folder (as Windows does for a bare
// name) or a relative PATH entry: HERMES_TUI_GATEWAY_BIN, then a build in the checkout this
// client runs from (root, never the working folder), then the rooted PATH entries.
const resolveGatewayBinary = (root: string): { bin: string } | { problem: string } => {
  const configured = process.env.HERMES_TUI_GATEWAY_BIN?.trim()

  if (configured) {
    return isRootedPath(configured)
      ? { bin: configured }
      : { problem: `HERMES_TUI_GATEWAY_BIN must be an absolute path, not ${configured}` }
  }

  const pathDirs = (process.env.PATH ?? '').split(delimiter).filter(isRootedPath)
  const bin = [resolve(root, 'target', 'release'), resolve(root, 'target', 'debug'), ...pathDirs]
    .map(dir => join(dir, GATEWAY_BIN))
    .find(p => existsSync(p))

  return bin
    ? { bin }
    : { problem: `${GATEWAY_BIN} not found in ${resolve(root, 'target')} or on PATH (${GATEWAY_BUILD_HINT})` }
}

const asGatewayEvent = (value: unknown): GatewayEvent | null =>
  value && typeof value === 'object' && !Array.isArray(value) && typeof (value as { type?: unknown }).type === 'string'
    ? (value as GatewayEvent)
    : null

// Hoisted decoder: attach mode can drive high-frequency binary frames
// (tool deltas, reasoning streams) and constructing a fresh TextDecoder
// per message creates avoidable GC pressure. One module-level instance
// is fine because UTF-8 is stateless and we always pass entire frames.
const _wireDecoder = new TextDecoder()

const asWireText = (raw: unknown): string | null => {
  if (typeof raw === 'string') {
    return raw
  }

  if (raw instanceof ArrayBuffer) {
    return _wireDecoder.decode(raw)
  }

  if (ArrayBuffer.isView(raw)) {
    return _wireDecoder.decode(raw)
  }

  return null
}

// Matches `<scheme>://user:pass@host…` style user-info segments in
// otherwise-malformed URLs that the WHATWG `URL` parser can't accept.
// Used by the `redactUrl` fallback so embedded credentials are
// scrubbed from log lines even when the URL is unparseable.
const _USERINFO_FALLBACK_RE = /^([a-z][a-z0-9+.-]*:\/\/)[^/?#@]*@/i

// Connection URLs (gateway, sidecar) often carry bearer tokens in the query
// string. We surface them in user-facing log lines and the
// `gateway.start_timeout` payload, so always strip the query string and any
// embedded user-info before logging.
const redactUrl = (raw: string): string => {
  if (!raw) {
    return raw
  }

  try {
    const url = new URL(raw)
    const userInfo = url.username || url.password ? '***@' : ''
    const query = url.search ? '?***' : ''

    return `${url.protocol}//${userInfo}${url.host}${url.pathname}${query}`
  } catch {
    // WHATWG URL rejected the input. Best-effort: strip an embedded
    // `user:pass@` segment AND the query string so a malformed token
    // bearer can never escape into the log tail.
    const noUserInfo = raw.replace(_USERINFO_FALLBACK_RE, '$1***@')
    const queryIdx = noUserInfo.indexOf('?')

    return queryIdx >= 0 ? `${noUserInfo.slice(0, queryIdx)}?***` : noUserInfo
  }
}

interface Pending {
  id: string
  method: string
  reject: (e: Error) => void
  resolve: (v: unknown) => void
  timeout: ReturnType<typeof setTimeout>
}

export class GatewayClient extends EventEmitter {
  private proc: ChildProcess | null = null
  private ws: WebSocket | null = null
  private wsConnectPromise: Promise<void> | null = null
  private sidecarWs: WebSocket | null = null
  private attachUrl: null | string = null
  private sidecarUrl: null | string = null
  private reqId = 0
  private logs = new CircularBuffer<string>(MAX_GATEWAY_LOG_LINES)
  private pending = new Map<string, Pending>()
  private bufferedEvents = new CircularBuffer<GatewayEvent>(MAX_BUFFERED_EVENTS)
  private pendingExit: number | null | undefined
  private ready = false
  private readyTimer: ReturnType<typeof setTimeout> | null = null
  private subscribed = false
  private stdoutRl: ReturnType<typeof createInterface> | null = null
  private stderrRl: ReturnType<typeof createInterface> | null = null

  constructor() {
    super()
    // useInput / createGatewayEventHandler can legitimately attach many
    // listeners. Default 10-cap triggers spurious warnings.
    this.setMaxListeners(0)
  }

  private publish(ev: GatewayEvent) {
    if (ev.type === 'gateway.ready') {
      this.ready = true

      if (this.readyTimer) {
        clearTimeout(this.readyTimer)
        this.readyTimer = null
      }
    }

    if (this.subscribed) {
      return void this.emit('event', ev)
    }

    this.bufferedEvents.push(ev)
  }

  private clearReadyTimer() {
    if (this.readyTimer) {
      clearTimeout(this.readyTimer)
      this.readyTimer = null
    }
  }

  private closeSidecarSocket() {
    try {
      this.sidecarWs?.close()
    } catch {
      // best effort
    } finally {
      this.sidecarWs = null
    }
  }

  private closeGatewaySocket() {
    // Null the active reference BEFORE invoking close(): real WebSocket
    // implementations dispatch the 'close' event after a microtask hop,
    // so by the time the handler runs `this.ws` should already be null
    // and the identity guard will correctly classify the close as
    // belonging to a discarded socket. (Test fakes emit synchronously,
    // so doing the swap up front is also what makes the identity guard
    // match real timing in tests.)
    const ws = this.ws
    this.ws = null
    this.wsConnectPromise = null
    try {
      ws?.close()
    } catch {
      // best effort
    }
  }

  private resetStartupState() {
    // Reject any in-flight RPCs left over from the previous transport
    // before we swap. Otherwise the old transport's stale exit/close
    // handlers (now identity-gated to ignore unrelated transports)
    // never fire `rejectPending`, leaving callers hanging on promises
    // attached to a discarded child / socket.
    this.rejectPending(new Error('gateway restarting'))
    this.ready = false
    this.bufferedEvents.clear()
    this.pendingExit = undefined
    this.stdoutRl?.close()
    this.stderrRl?.close()
    this.stdoutRl = null
    this.stderrRl = null
    this.clearReadyTimer()
  }

  private startReadyTimer(command: string, cwd: string) {
    this.readyTimer = setTimeout(() => {
      if (this.ready) {
        return
      }

      // Append the most recent gateway stderr/log lines to the timeout
      // event so users can tell apart "wrong python", "missing dep",
      // and "config parse failure" from one glance instead of having
      // to dig through `/logs`.  Capped to keep the activity feed
      // readable on slow boots.
      const stderrTail = this.getLogTail(20)

      this.pushLog(`[startup] timed out waiting for gateway.ready (command=${command}, cwd=${cwd})`)
      this.publish({
        type: 'gateway.start_timeout',
        payload: { command, cwd, stderr_tail: stderrTail }
      })

      // A spawned gateway that never announced its port is stopped; its exit then rejects
      // the requests waiting for it.
      if (this.proc && !this.ws) {
        this.proc.kill()
      }
    }, STARTUP_TIMEOUT_MS)
  }

  private handleTransportExit(code: null | number, reason?: string) {
    this.clearReadyTimer()
    this.closeSidecarSocket()
    this.rejectPending(new Error(reason || `gateway exited${code === null ? '' : ` (${code})`}`))

    if (this.subscribed) {
      this.emit('exit', code)
    } else {
      this.pendingExit = code
    }
  }

  private connectSidecarMirror() {
    this.closeSidecarSocket()

    if (!this.sidecarUrl) {
      return
    }

    if (typeof WebSocket === 'undefined') {
      this.pushLog(`[sidecar] WebSocket unavailable; skipping mirror to ${redactUrl(this.sidecarUrl)}`)
      return
    }

    try {
      const ws = new WebSocket(this.sidecarUrl)

      this.sidecarWs = ws
      ws.addEventListener('close', () => {
        if (this.sidecarWs === ws) {
          this.sidecarWs = null
        }
      })
      ws.addEventListener('error', () => {
        this.pushLog('[sidecar] mirror connection error')
      })
    } catch (err) {
      this.pushLog(`[sidecar] failed to connect ${redactUrl(this.sidecarUrl)} (constructor error)`)
      this.sidecarWs = null
    }
  }

  private mirrorEventToSidecar(rawFrame: string) {
    const ws = this.sidecarWs

    if (!ws || ws.readyState !== WS_OPEN) {
      return
    }

    try {
      ws.send(rawFrame)
    } catch {
      // best effort
    }
  }

  private handleWebSocketFrame(raw: unknown) {
    const text = asWireText(raw)

    if (!text) {
      return
    }

    try {
      const frame = JSON.parse(text) as Record<string, unknown>

      if (frame.method === 'event') {
        this.mirrorEventToSidecar(text)
      }

      this.dispatch(frame)
    } catch {
      const preview = text.trim().slice(0, MAX_LOG_PREVIEW) || '(empty frame)'

      this.pushLog(`[protocol] malformed websocket frame: ${preview}`)
      this.publish({ type: 'gateway.protocol_error', payload: { preview } })
    }
  }

  // Starts the Rust gateway on a free loopback port and attaches to it with a token made here,
  // which reaches the child through its environment, never its arguments.
  private startSpawnedGateway(root: string) {
    const found = resolveGatewayBinary(root)

    if ('problem' in found) {
      const line = `[spawn] ${found.problem}`
      const missing = Promise.reject(new Error(found.problem))

      missing.catch(() => {})
      this.wsConnectPromise = missing
      this.pushLog(line)
      this.publish({ type: 'gateway.stderr', payload: { line } })
      this.handleTransportExit(1, found.problem)

      return
    }

    const { bin } = found
    const cwd = process.env.HERMES_CWD || root
    const token = randomBytes(32).toString('hex')
    const env = { ...process.env, [GATEWAY_TOKEN_ENV]: token }

    this.startReadyTimer(bin, cwd)
    this.proc = spawn(bin, ['--host', '127.0.0.1', '--port', '0'], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'] })

    const ownedProc = this.proc

    this.stdoutRl = createInterface({ input: this.proc.stdout! })

    const announced = new Promise<string>((resolve, reject) => {
      this.stdoutRl!.on('line', raw => {
        const line = raw.trim()
        const port = GATEWAY_READY_RE.exec(line)?.[1]

        if (port) {
          resolve(port)
        } else if (line) {
          this.pushLog(line)
        }
      })
      ownedProc.once('error', reject)
      ownedProc.once('exit', code => reject(new Error(`gateway exited (${code}) before it was ready`)))
    })

    const connected = announced.then(port => {
      if (this.proc !== ownedProc) {
        throw new Error('gateway restarting')
      }

      this.wsConnectPromise = null
      this.startAttachedGateway(`ws://127.0.0.1:${port}/api/ws?token=${token}`)

      return this.wsConnectPromise ?? Promise.reject(new Error('gateway websocket startup failed'))
    })

    connected.catch(() => {})
    this.wsConnectPromise = connected

    this.stderrRl = createInterface({ input: this.proc.stderr! })
    this.stderrRl.on('line', raw => {
      const line = truncateLine(raw.trim())

      if (!line) {
        return
      }

      this.pushLog(line)
      this.publish({ type: 'gateway.stderr', payload: { line } })
    })

    this.proc.on('error', err => {
      // Skip stale errors on an already-replaced child.
      if (this.proc !== ownedProc) {
        return
      }

      const hint = (err as NodeJS.ErrnoException).code === 'ENOENT' ? ` (${GATEWAY_BUILD_HINT})` : ''
      const line = `[spawn] ${err.message}${hint}`

      this.pushLog(line)
      this.publish({ type: 'gateway.stderr', payload: { line } })
      // Detach the reference up front so the late `exit` event for
      // this same child is identity-skipped (we don't want to emit
      // 'exit' twice). Then run the full teardown — clears the
      // startup timer so we don't fire a misleading
      // `gateway.start_timeout`, rejects pending RPCs, and emits or
      // queues a single `exit`.
      this.proc = null
      this.handleTransportExit(1, `gateway error: ${err.message}`)
    })
    this.proc.on('exit', code => {
      // start() can replace `this.proc` while an old child is still
      // tearing down. Skip stale exits so we don't clear the new
      // startup timer or reject newly-issued pending requests.
      if (this.proc !== ownedProc) {
        return
      }

      // Drop the socket first so its close does not report the exit a second time.
      this.closeGatewaySocket()
      this.handleTransportExit(code)
    })
  }

  private startAttachedGateway(attachUrl: string) {
    const safeAttachUrl = redactUrl(attachUrl)

    if (typeof WebSocket === 'undefined') {
      const line = `[startup] WebSocket API unavailable; cannot attach to ${safeAttachUrl}`

      this.pushLog(line)
      this.publish({ type: 'gateway.stderr', payload: { line } })
      this.handleTransportExit(1, 'gateway websocket unavailable')

      return
    }

    try {
      const ws = new WebSocket(attachUrl)
      let settled = false

      this.ws = ws
      const connectPromise = new Promise<void>((resolve, reject) => {
        ws.addEventListener(
          'open',
          () => {
            if (!settled) {
              settled = true
              resolve()
            }

            this.connectSidecarMirror()
          },
          { once: true }
        )

        ws.addEventListener(
          'error',
          () => {
            if (!settled) {
              this.pushLog('[startup] gateway websocket connect error')
              settled = true
              reject(new Error('gateway websocket connection failed'))
            }
          },
          { once: true }
        )
        ws.addEventListener(
          'close',
          ev => {
            if (!settled) {
              settled = true
              reject(new Error(`gateway websocket closed (${ev.code}) during connect`))
            }
          },
          { once: true }
        )
      })

      // The connect promise is only awaited by RPCs that arrive while
      // the socket is still connecting. If no request races the open
      // (or a teardown drops the reference before anyone observes it),
      // a connect-error / early-close rejection would surface as an
      // unhandled promise rejection in Node. Attach a no-op handler to
      // ensure the rejection is always observed.
      connectPromise.catch(() => {})
      this.wsConnectPromise = connectPromise

      ws.addEventListener('message', ev => this.handleWebSocketFrame(ev.data))
      ws.addEventListener('close', ev => {
        // Skip close events from sockets that have already been
        // replaced — start() / closeGatewaySocket() can swap `this.ws`
        // before an in-flight close lands, and we must not clear the
        // new ready timer or reject the new pending requests on behalf
        // of a stale socket.
        if (this.ws !== ws) {
          return
        }

        this.ws = null
        this.wsConnectPromise = null

        // A gateway this client spawned is stopped with its socket; dropping the reference
        // first makes its later exit event a stale one, so the exit is reported once.
        const proc = this.proc

        this.proc = null
        proc?.kill()
        this.handleTransportExit(ev.code, `gateway websocket closed${ev.code ? ` (${ev.code})` : ''}`)
      })
      ws.addEventListener('error', () => {
        // A socket this client already dropped is not reported.
        if (this.ws !== ws) {
          return
        }

        const line = '[gateway] websocket transport error'

        this.pushLog(line)
        this.publish({ type: 'gateway.stderr', payload: { line } })
      })
    } catch (err) {
      this.pushLog(`[startup] failed to connect websocket gateway ${safeAttachUrl} (constructor error)`)
      this.handleTransportExit(1, 'gateway websocket startup failed')
    }
  }

  start() {
    const root = process.env.HERMES_PYTHON_SRC_ROOT ?? resolve(import.meta.dirname, '../../')
    const attachUrl = resolveGatewayAttachUrl()
    const sidecarUrl = resolveSidecarUrl()

    this.attachUrl = attachUrl
    this.sidecarUrl = sidecarUrl
    this.resetStartupState()

    if (this.proc && !this.proc.killed && this.proc.exitCode === null) {
      this.proc.kill()
    }
    this.proc = null
    this.closeGatewaySocket()
    this.closeSidecarSocket()

    if (attachUrl) {
      this.startReadyTimer('websocket', redactUrl(attachUrl))
      this.startAttachedGateway(attachUrl)
      return
    }

    this.startSpawnedGateway(root)
  }

  private dispatch(msg: Record<string, unknown>) {
    const id = msg.id as string | undefined
    const p = id ? this.pending.get(id) : undefined

    if (p) {
      this.settle(p, msg.error ? this.toError(msg.error) : null, msg.result)

      return
    }

    if (msg.method === 'event') {
      const ev = asGatewayEvent(msg.params)

      if (ev) {
        this.publish(ev)
      }
    }
  }

  private toError(raw: unknown): Error {
    const err = raw as { message?: unknown } | null | undefined

    return new Error(typeof err?.message === 'string' ? err.message : 'request failed')
  }

  private settle(p: Pending, err: Error | null, result: unknown) {
    clearTimeout(p.timeout)
    this.pending.delete(p.id)

    if (err) {
      p.reject(err)
    } else {
      p.resolve(result)
    }
  }

  private pushLog(line: string) {
    this.logs.push(truncateLine(line))
  }

  private rejectPending(err: Error) {
    for (const p of this.pending.values()) {
      clearTimeout(p.timeout)
      p.reject(err)
    }

    this.pending.clear()
  }

  // Arrow class-field — stable identity, so `setTimeout(this.onTimeout, …, id)`
  // doesn't allocate a bound function per request.
  private onTimeout = (id: string) => {
    const p = this.pending.get(id)

    if (p) {
      this.pending.delete(id)
      p.reject(new Error(`timeout: ${p.method}`))
    }
  }

  drain() {
    this.subscribed = true

    for (const ev of this.bufferedEvents.drain()) {
      this.emit('event', ev)
    }

    if (this.pendingExit !== undefined) {
      const code = this.pendingExit

      this.pendingExit = undefined
      this.emit('exit', code)
    }
  }

  getLogTail(limit = 20): string {
    return this.logs.tail(Math.max(1, limit)).join('\n')
  }

  private async ensureAttachedWebSocket(method: string): Promise<WebSocket> {
    // A spawned gateway has a connect promise before it has a socket.
    if (!this.wsConnectPromise || this.ws?.readyState === WS_CLOSED || this.ws?.readyState === WS_CLOSING) {
      this.start()
    }

    try {
      await this.wsConnectPromise
    } catch (err) {
      throw err instanceof Error ? err : new Error(String(err))
    }

    if (!this.ws || this.ws.readyState !== WS_OPEN) {
      throw new Error(`gateway not connected: ${method}`)
    }

    return this.ws
  }

  private requestOverWebSocket<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    return this.ensureAttachedWebSocket(method).then(
      ws =>
        new Promise<T>((resolve, reject) => {
          const id = `r${++this.reqId}`
          const timeout = setTimeout(this.onTimeout, REQUEST_TIMEOUT_MS, id)

          timeout.unref?.()
          this.pending.set(id, {
            id,
            method,
            reject,
            resolve: v => resolve(v as T),
            timeout
          })

          try {
            ws.send(JSON.stringify({ id, jsonrpc: '2.0', method, params }))
          } catch (e) {
            const pending = this.pending.get(id)

            if (pending) {
              clearTimeout(pending.timeout)
              this.pending.delete(id)
            }

            reject(e instanceof Error ? e : new Error(String(e)))
          }
        })
    )
  }

  request<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    if (resolveGatewayAttachUrl() !== this.attachUrl) {
      // HERMES_TUI_GATEWAY_URL changed at runtime: restart the transport so a gateway
      // this client spawned is stopped rather than left running beside the new one.
      this.rejectPending(new Error('gateway attach url changed'))
      this.start()
    }

    return this.requestOverWebSocket<T>(method, params)
  }

  kill() {
    this.proc?.kill()
    this.closeGatewaySocket()
    this.closeSidecarSocket()
    this.clearReadyTimer()
    // The ws 'close' handler is identity-gated on `this.ws === ws`
    // and we just nulled `this.ws`, so it will short-circuit and
    // skip handleTransportExit. Reject pending RPCs explicitly so
    // attach-mode promises do not hang after an intentional kill.
    this.rejectPending(new Error('gateway closed'))
  }
}
