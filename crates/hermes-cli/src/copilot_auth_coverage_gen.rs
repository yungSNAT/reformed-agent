use super::*;

#[test]
fn encode_keeps_unreserved_characters_unchanged() {
    let input = "ABCXYZabcxyz0123456789-_.~";
    assert_eq!(super::urlencoding::encode_query_component(input), input);
}

#[test]
fn encode_empty_string_is_empty() {
    assert_eq!(super::urlencoding::encode_query_component(""), "");
}

#[test]
fn encode_escapes_reserved_and_space_characters() {
    assert_eq!(super::urlencoding::encode_query_component(" "), "%20");
    assert_eq!(super::urlencoding::encode_query_component(":"), "%3A");
    assert_eq!(super::urlencoding::encode_query_component("/"), "%2F");
    assert_eq!(super::urlencoding::encode_query_component("&="), "%26%3D");
    assert_eq!(super::urlencoding::encode_query_component("+"), "%2B");
    assert_eq!(
        super::urlencoding::encode_query_component("a b:c"),
        "a%20b%3Ac"
    );
}

#[test]
fn encode_escapes_multibyte_utf8_as_uppercase_hex_bytes() {
    assert_eq!(super::urlencoding::encode_query_component("é"), "%C3%A9");
    assert_eq!(
        super::urlencoding::encode_query_component("\u{1F600}"),
        "%F0%9F%98%80"
    );
}

#[test]
fn encode_escapes_control_bytes_with_two_hex_digits() {
    assert_eq!(super::urlencoding::encode_query_component("\n"), "%0A");
    assert_eq!(super::urlencoding::encode_query_component("\0"), "%00");
}

#[test]
fn form_body_empty_pairs_is_empty_string() {
    assert_eq!(form_body(&[]), "");
}

#[test]
fn form_body_single_pair() {
    assert_eq!(form_body(&[("client_id", "abc123")]), "client_id=abc123");
}

#[test]
fn form_body_joins_pairs_with_ampersand_and_encodes_values() {
    let body = form_body(&[
        ("client_id", DEFAULT_DEVICE_CLIENT_ID),
        ("scope", "read:org read:user repo"),
    ]);
    assert_eq!(
        body,
        "client_id=Iv23liCLQ87g8RVSjNGp&scope=read%3Aorg%20read%3Auser%20repo"
    );
}

#[test]
fn form_body_encodes_keys_and_grant_type() {
    let body = form_body(&[
        ("device code", "x&y"),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ]);
    assert_eq!(
        body,
        "device%20code=x%26y&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"
    );
}

#[test]
fn form_body_preserves_pair_order() {
    let body = form_body(&[("b", "2"), ("a", "1"), ("c", "3")]);
    assert_eq!(body, "b=2&a=1&c=3");
}
