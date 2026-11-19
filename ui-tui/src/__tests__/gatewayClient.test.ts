import { spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import { existsSync, mkdtempSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { PassThrough } from 'node:stream'

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

import { GatewayClient } from '../gatewayClient.js'

// Real spawn unless a test replaces it with mockImplementationOnce.
vi.mock('node:child_process', async importOriginal => {
  const actual = await importOriginal<typeof import('node:child_process')>()

  return { ...actual, spawn: vi.fn(actual.spawn) }
})

interface ListenerEntry {
  callback: (event: any) => void
  once: boolean
}

class FakeWebSocket {
  static CONNECTING = 0
  static OPEN = 1
  static CLOSING = 2
  static CLOSED = 3
  static instances: FakeWebSocket[] = []

  readyState = FakeWebSocket.CONNECTING
  sent: string[] = []
  readonly url: string
  private listeners = new Map<string, ListenerEntry[]>()

  constructor(url: string) {
    this.url = url
    FakeWebSocket.instances.push(this)
  }

  static reset() {
    FakeWebSocket.instances = []
  }

  addEventListener(type: string, callback: (event: any) => void, options?: unknown) {
    const once =
      typeof options === 'object' &&
      options !== null &&
      'once' in options &&
      Boolean((options as { once?: unknown }).once)
    const entries = this.listeners.get(type) ?? []

    entries.push({ callback, once })
    this.listeners.set(type, entries)
  }

  removeEventListener(type: string, callback: (event: any) => void) {
    const entries = this.listeners.get(type)

    if (!entries) {
      return
    }

    this.listeners.set(
      type,
      entries.filter(entry => entry.callback !== callback)
    )
  }

  send(payload: string) {
    if (this.readyState !== FakeWebSocket.OPEN) {
      throw new Error('socket not open')
    }

    this.sent.push(payload)
  }

  close(code = 1000) {
    if (this.readyState === FakeWebSocket.CLOSED) {
      return
    }

    this.readyState = FakeWebSocket.CLOSED
    this.emit('close', { code })
  }

  open() {
    this.readyState = FakeWebSocket.OPEN
    this.emit('open', {})
  }

  message(data: string) {
    this.emit('message', { data })
  }

  private emit(type: string, event: any) {
    const entries = [...(this.listeners.get(type) ?? [])]

    for (const entry of entries) {
      entry.callback(event)
      if (entry.once) {
        this.removeEventListener(type, entry.callback)
      }
    }
  }
}

describe('GatewayClient websocket attach mode', () => {
  const originalWebSocket = globalThis.WebSocket
  let originalGatewayUrl: string | undefined
  let originalSidecarUrl: string | undefined

  beforeEach(() => {
    originalGatewayUrl = process.env.HERMES_TUI_GATEWAY_URL
    originalSidecarUrl = process.env.HERMES_TUI_SIDECAR_URL
    FakeWebSocket.reset()
    ;(globalThis as { WebSocket?: unknown }).WebSocket = FakeWebSocket as unknown as typeof WebSocket
  })

  afterEach(() => {
    if (originalGatewayUrl === undefined) {
      delete process.env.HERMES_TUI_GATEWAY_URL
    } else {
      process.env.HERMES_TUI_GATEWAY_URL = originalGatewayUrl
    }

    if (originalSidecarUrl === undefined) {
      delete process.env.HERMES_TUI_SIDECAR_URL
    } else {
      process.env.HERMES_TUI_SIDECAR_URL = originalSidecarUrl
    }

    FakeWebSocket.reset()

    if (originalWebSocket) {
      globalThis.WebSocket = originalWebSocket
    } else {
      delete (globalThis as { WebSocket?: unknown }).WebSocket
    }
  })

  it('waits for websocket open and resolves RPC requests', async () => {
    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway.test/api/ws?token=abc'
    const gw = new GatewayClient()

    gw.start()
    const gatewaySocket = FakeWebSocket.instances[0]!
    const req = gw.request<{ ok: boolean }>('session.create', { cols: 80 })

    expect(gatewaySocket.sent).toHaveLength(0)
    gatewaySocket.open()
    await vi.waitFor(() => expect(gatewaySocket.sent).toHaveLength(1))

    const frame = JSON.parse(gatewaySocket.sent[0] ?? '{}') as { id: string; method: string }
    expect(frame.method).toBe('session.create')

    gatewaySocket.message(JSON.stringify({ id: frame.id, jsonrpc: '2.0', result: { ok: true } }))
    await expect(req).resolves.toEqual({ ok: true })

    gw.kill()
  })

  it('mirrors event frames to sidecar websocket when configured', async () => {
    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway.test/api/ws?token=abc'
    process.env.HERMES_TUI_SIDECAR_URL = 'ws://gateway.test/api/pub?token=abc&channel=demo'

    const gw = new GatewayClient()
    const seen: string[] = []

    gw.on('event', ev => seen.push(ev.type))
    gw.start()

    const gatewaySocket = FakeWebSocket.instances[0]!
    gatewaySocket.open()
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(2))

    const sidecarSocket = FakeWebSocket.instances[1]!

    sidecarSocket.open()
    gw.drain()

    const eventFrame = JSON.stringify({
      jsonrpc: '2.0',
      method: 'event',
      params: { type: 'tool.start', payload: { tool_id: 't1' } }
    })
    gatewaySocket.message(eventFrame)

    expect(seen).toContain('tool.start')
    expect(sidecarSocket.sent).toContain(eventFrame)

    gw.kill()
  })

  it('emits exit when attached websocket closes', () => {
    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway.test/api/ws?token=abc'
    const gw = new GatewayClient()
    const exits: Array<null | number> = []

    gw.on('exit', code => exits.push(code))
    gw.start()

    const gatewaySocket = FakeWebSocket.instances[0]!

    gatewaySocket.open()
    gw.drain()
    gatewaySocket.close(1011)

    expect(exits).toEqual([1011])
  })

  it('rejects pending RPCs with websocket wording when the attached socket closes', async () => {
    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway.test/api/ws?token=abc'
    const gw = new GatewayClient()

    gw.start()
    const gatewaySocket = FakeWebSocket.instances[0]!

    gatewaySocket.open()
    gw.drain()

    const req = gw.request('session.create', {})
    await vi.waitFor(() => expect(gatewaySocket.sent.length).toBeGreaterThan(0))

    gatewaySocket.close(1011)

    await expect(req).rejects.toThrow(/gateway websocket closed \(1011\)/)
  })

  it('rejects pending RPCs when kill() closes the attached websocket', async () => {
    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway.test/api/ws?token=abc'
    const gw = new GatewayClient()

    gw.start()
    const gatewaySocket = FakeWebSocket.instances[0]!

    gatewaySocket.open()
    gw.drain()

    const req = gw.request('session.create', {})
    await vi.waitFor(() => expect(gatewaySocket.sent.length).toBeGreaterThan(0))

    gw.kill()

    await expect(req).rejects.toThrow(/gateway closed/)
  })

  it('reattaches when HERMES_TUI_GATEWAY_URL rotates between requests', async () => {
    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway-old.test/api/ws?token=abc'
    const gw = new GatewayClient()

    gw.start()
    const firstSocket = FakeWebSocket.instances[0]!

    firstSocket.open()
    gw.drain()

    const stale = gw.request('session.create', {})
    await vi.waitFor(() => expect(firstSocket.sent.length).toBeGreaterThan(0))

    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway-new.test/api/ws?token=xyz'
    const next = gw.request('session.create', {})

    await expect(stale).rejects.toThrow(/gateway attach url changed/)
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(2))

    const secondSocket = FakeWebSocket.instances[1]!
    expect(secondSocket.url).toContain('gateway-new.test')

    secondSocket.open()
    await vi.waitFor(() => expect(secondSocket.sent.length).toBeGreaterThan(0))

    const frame = JSON.parse(secondSocket.sent[0] ?? '{}') as { id: string }
    secondSocket.message(JSON.stringify({ id: frame.id, jsonrpc: '2.0', result: { ok: true } }))

    await expect(next).resolves.toEqual({ ok: true })
    gw.kill()
  })

  it('redacts query string secrets in attach failure logs and events', () => {
    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway.test/api/ws?token=hunter2&channel=secret'
    delete (globalThis as { WebSocket?: unknown }).WebSocket

    const gw = new GatewayClient()
    const stderrLines: string[] = []

    gw.on('event', ev => {
      if (ev.type === 'gateway.stderr' && typeof ev.payload?.line === 'string') {
        stderrLines.push(ev.payload.line)
      }
    })
    gw.start()
    gw.drain()

    expect(stderrLines.length).toBeGreaterThan(0)
    for (const line of stderrLines) {
      expect(line).not.toContain('hunter2')
      expect(line).not.toContain('channel=secret')
    }

    expect(gw.getLogTail(20)).not.toContain('hunter2')
    expect(gw.getLogTail(20)).not.toContain('channel=secret')

    gw.kill()
  })

  it('redacts attach URL secrets when the WebSocket constructor throws', () => {
    const secretUrl = 'ws://gateway.test/api/ws?token=hunter2&channel=secret'

    process.env.HERMES_TUI_GATEWAY_URL = secretUrl
    ;(globalThis as { WebSocket?: unknown }).WebSocket = class ThrowingWebSocket extends FakeWebSocket {
      constructor(url: string) {
        throw new TypeError(`Invalid URL: ${url}`)
      }
    } as unknown as typeof WebSocket

    const gw = new GatewayClient()

    gw.start()
    gw.drain()

    const tail = gw.getLogTail(20)
    expect(tail).not.toContain('hunter2')
    expect(tail).not.toContain('channel=secret')
    expect(tail).not.toContain(secretUrl)
    expect(tail).toContain('ws://gateway.test/api/ws?***')

    gw.kill()
  })

  it('redacts sidecar URL secrets when the WebSocket constructor throws', async () => {
    const sidecarUrl = 'ws://gateway.test/api/pub?token=hunter2&channel=secret'

    process.env.HERMES_TUI_GATEWAY_URL = 'ws://gateway.test/api/ws?token=abc'
    process.env.HERMES_TUI_SIDECAR_URL = sidecarUrl
    ;(globalThis as { WebSocket?: unknown }).WebSocket = class ThrowingSidecarWebSocket extends FakeWebSocket {
      constructor(url: string) {
        if (url.includes('/api/pub')) {
          throw new TypeError(`Invalid URL: ${url}`)
        }

        super(url)
      }
    } as unknown as typeof WebSocket

    const gw = new GatewayClient()

    gw.start()
    const gatewaySocket = FakeWebSocket.instances[0]!
    gatewaySocket.open()
    await vi.waitFor(() => expect(gw.getLogTail(20)).toContain('[sidecar] failed to connect'))

    const tail = gw.getLogTail(20)
    expect(tail).not.toContain('hunter2')
    expect(tail).not.toContain('channel=secret')
    expect(tail).not.toContain(sidecarUrl)
    expect(tail).toContain('ws://gateway.test/api/pub?***')

    gw.kill()
  })

  it('redacts user-info credentials even on URLs the WHATWG parser rejects', () => {
    // Port 99999 is outside the WHATWG URL parser's valid 0–65535
    // range and survives `.trim()`, so the fixture deterministically
    // exercises `redactUrl()`'s fallback branch across Node versions.
    // (An earlier `%zz` user-info fixture did NOT actually throw in
    // recent Node — WHATWG accepts malformed percent escapes there —
    // which silently routed the test through the structured-URL path.)
    const fixture = 'ws://alice:hunter2@gateway.test:99999/api/ws?token=secret'
    expect(() => new URL(fixture)).toThrow()

    process.env.HERMES_TUI_GATEWAY_URL = fixture
    delete (globalThis as { WebSocket?: unknown }).WebSocket

    const gw = new GatewayClient()
    const stderrLines: string[] = []

    gw.on('event', ev => {
      if (ev.type === 'gateway.stderr' && typeof ev.payload?.line === 'string') {
        stderrLines.push(ev.payload.line)
      }
    })
    gw.start()
    gw.drain()

    expect(stderrLines.length).toBeGreaterThan(0)
    for (const line of stderrLines) {
      expect(line).not.toContain('alice')
      expect(line).not.toContain('hunter2')
      expect(line).not.toContain('token=secret')
    }

    const tail = gw.getLogTail(20)
    expect(tail).not.toContain('alice')
    expect(tail).not.toContain('hunter2')
    expect(tail).not.toContain('token=secret')

    gw.kill()
  })
})

class FakeChild extends EventEmitter {
  stdout = new PassThrough()
  stderr = new PassThrough()
  exitCode: null | number = null
  killed = false
  kill = vi.fn(() => {
    this.killed = true

    return true
  })
}

describe('GatewayClient spawned Rust gateway', () => {
  const originalWebSocket = globalThis.WebSocket
  const saved = new Map<string, string | undefined>()
  let child: FakeChild

  beforeEach(() => {
    for (const name of ['HERMES_TUI_GATEWAY_URL', 'HERMES_TUI_SIDECAR_URL', 'HERMES_TUI_GATEWAY_BIN']) {
      saved.set(name, process.env[name])
      delete process.env[name]
    }

    process.env.HERMES_TUI_GATEWAY_BIN = '/opt/hermes/hermes-desktop-gateway'
    child = new FakeChild()
    vi.mocked(spawn).mockImplementationOnce((() => child) as unknown as typeof spawn)
    FakeWebSocket.reset()
    ;(globalThis as { WebSocket?: unknown }).WebSocket = FakeWebSocket as unknown as typeof WebSocket
  })

  afterEach(() => {
    for (const [name, value] of saved) {
      if (value === undefined) {
        delete process.env[name]
      } else {
        process.env[name] = value
      }
    }

    vi.mocked(spawn).mockClear()
    FakeWebSocket.reset()
    globalThis.WebSocket = originalWebSocket
  })

  it('starts the Rust gateway and attaches with the token it gave it', async () => {
    const gw = new GatewayClient()

    gw.start()

    expect(spawn).toHaveBeenCalledTimes(1)
    const [command, args, options] = vi.mocked(spawn).mock.calls[0] as unknown as [
      string,
      string[],
      { env: Record<string, string> }
    ]

    expect(command).toBe('/opt/hermes/hermes-desktop-gateway')
    expect(args).toEqual(['--host', '127.0.0.1', '--port', '0'])

    const token = options.env.HERMES_DASHBOARD_SESSION_TOKEN ?? ''

    expect(token).toMatch(/^[0-9a-f]{64}$/)

    const req = gw.request<{ ok: boolean }>('setup.runtime_check')

    child.stdout.write('HERMES_BACKEND_READY port=41234\nHERMES_DASHBOARD_READY port=41234\n')
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(1))

    const socket = FakeWebSocket.instances[0]!

    expect(socket.url).toBe(`ws://127.0.0.1:41234/api/ws?token=${token}`)
    socket.open()
    await vi.waitFor(() => expect(socket.sent).toHaveLength(1))

    const frame = JSON.parse(socket.sent[0] ?? '{}') as { id: string; method: string }

    expect(frame.method).toBe('setup.runtime_check')
    socket.message(JSON.stringify({ id: frame.id, jsonrpc: '2.0', result: { ok: true } }))
    await expect(req).resolves.toEqual({ ok: true })
    expect(gw.getLogTail(50)).not.toContain(token)

    gw.kill()
    expect(child.kill).toHaveBeenCalled()
  })

  it('rejects requests and reports one exit when the gateway exits before it is ready', async () => {
    const gw = new GatewayClient()
    const exits: Array<null | number> = []

    gw.on('exit', code => exits.push(code))
    gw.start()
    gw.drain()

    const req = gw.request('setup.runtime_check')

    child.stderr.write('cannot load the Hermes config: bad yaml\n')
    child.exitCode = 1
    child.emit('exit', 1)

    await expect(req).rejects.toThrow(/gateway exited/)
    expect(exits).toEqual([1])
    await vi.waitFor(() => expect(gw.getLogTail(5)).toContain('cannot load the Hermes config'))
    expect(FakeWebSocket.instances).toHaveLength(0)
  })

  it('says how to build the gateway when its binary is missing', async () => {
    const gw = new GatewayClient()
    const lines: string[] = []
    const exits: Array<null | number> = []

    gw.on('event', ev => {
      if (ev.type === 'gateway.stderr' && typeof ev.payload?.line === 'string') {
        lines.push(ev.payload.line)
      }
    })
    gw.on('exit', code => exits.push(code))
    gw.start()
    gw.drain()

    const req = gw.request('setup.runtime_check')

    child.emit('error', Object.assign(new Error('spawn /opt/hermes/hermes-desktop-gateway ENOENT'), { code: 'ENOENT' }))

    await expect(req).rejects.toThrow(/ENOENT/)
    expect(lines.join('\n')).toContain('cargo build --release -p hermes-desktop-gateway')
    expect(exits).toEqual([1])
  })

  it('reports one exit when the gateway exits after attaching', async () => {
    const gw = new GatewayClient()
    const exits: Array<null | number> = []

    gw.on('exit', code => exits.push(code))
    gw.start()
    gw.drain()
    child.stdout.write('HERMES_BACKEND_READY port=41234\n')
    await vi.waitFor(() => expect(FakeWebSocket.instances).toHaveLength(1))

    const socket = FakeWebSocket.instances[0]!

    socket.open()
    child.exitCode = 0
    child.emit('exit', 0)
    socket.close(1006)

    expect(exits).toEqual([0])
  })
})

// Runs against a built gateway: HERMES_TUI_GATEWAY_IT_BIN=<path to hermes-desktop-gateway>.
const realGateway = process.env.HERMES_TUI_GATEWAY_IT_BIN?.trim()

describe.skipIf(!realGateway)('GatewayClient with the real Rust gateway', () => {
  const saved = new Map<string, string | undefined>()
  let home = ''

  beforeEach(() => {
    for (const name of ['HERMES_TUI_GATEWAY_URL', 'HERMES_TUI_SIDECAR_URL', 'HERMES_TUI_GATEWAY_BIN', 'HERMES_HOME']) {
      saved.set(name, process.env[name])
      delete process.env[name]
    }

    home = mkdtempSync(join(tmpdir(), 'hermes-tui-it-'))
    process.env.HERMES_HOME = home
    process.env.HERMES_TUI_GATEWAY_BIN = realGateway
  })

  afterEach(() => {
    for (const [name, value] of saved) {
      if (value === undefined) {
        delete process.env[name]
      } else {
        process.env[name] = value
      }
    }

    rmSync(home, { force: true, recursive: true })
  })

  it('starts the gateway, receives gateway.ready and answers a request', async () => {
    expect(existsSync(realGateway!)).toBe(true)

    const gw = new GatewayClient()
    const events: string[] = []
    const exits: Array<null | number> = []

    gw.on('event', ev => events.push(ev.type))
    gw.on('exit', code => exits.push(code))
    gw.start()
    gw.drain()

    const check = await gw.request<{ ok: boolean }>('setup.runtime_check')

    expect(typeof check.ok).toBe('boolean')
    expect(events).toContain('gateway.ready')

    gw.kill()
    await vi.waitFor(() => expect(exits).toHaveLength(1), { timeout: 10_000 })
  }, 30_000)
})
