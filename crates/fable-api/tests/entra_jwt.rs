//! Entra JWT validation and JWKS caching (spec §14 A-1) against a mocked
//! JWKS endpoint. Tokens are signed with test-only keys in `fixtures/`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fable_api::auth::{EntraSettings, EntraValidator, JwksCache, TokenError};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUER: &str = "https://login.microsoftonline.com/tenant-1/v2.0";
const AUDIENCE: &str = "11111111-2222-3333-4444-555555555555";
const JWKS_PATH: &str = "/tenant-1/discovery/v2.0/keys";

const KEY1_PEM: &[u8] = include_bytes!("fixtures/test-rsa-1.pem");
const KEY2_PEM: &[u8] = include_bytes!("fixtures/test-rsa-2.pem");
const KEY1_PUB: &str = include_str!("fixtures/test-rsa-1.pub.json");
const KEY2_PUB: &str = include_str!("fixtures/test-rsa-2.pub.json");

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn jwk(public: &str, kid: &str) -> Value {
    let p: Value = serde_json::from_str(public).unwrap();
    json!({
        "kty": "RSA", "use": "sig", "kid": kid, "n": p["n"], "e": p["e"],
        "x5t": kid, "x5c": ["ignored"], "issuer": ISSUER
    })
}

fn jwks(keys: &[Value]) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({ "keys": keys }))
}

fn claims() -> Value {
    json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "iat": now(),
        "nbf": now() - 10,
        "exp": now() + 3600,
        "oid": "user-oid",
        "sub": "pairwise-sub",
        "tid": "tenant-1",
        "name": "Ada Lovelace",
        "preferred_username": "ada@example.com",
        "groups": ["g1", "g2"],
        "roles": ["Rag.Admin"],
        "ver": "2.0"
    })
}

fn sign(pem: &[u8], kid: Option<&str>, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_owned);
    jsonwebtoken::encode(&header, claims, &EncodingKey::from_rsa_pem(pem).unwrap()).unwrap()
}

fn validator(server: &MockServer, ttl: Duration, cooldown: Duration) -> EntraValidator {
    let jwks_uri = format!("{}{JWKS_PATH}", server.uri());
    let settings = EntraSettings {
        issuer: ISSUER.to_owned(),
        audiences: vec![AUDIENCE.to_owned(), format!("api://{AUDIENCE}")],
        jwks_uri: jwks_uri.clone(),
    };
    let cache = JwksCache::with_timing(reqwest::Client::new(), jwks_uri, ttl, cooldown);
    EntraValidator::new(settings, cache)
}

const HOUR: Duration = Duration::from_secs(3600);

async fn server_with_key1(expected_fetches: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(jwks(&[jwk(KEY1_PUB, "k1")]))
        .expect(expected_fetches)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn valid_token_yields_identity_claims() {
    let server = server_with_key1(1).await;
    let v = validator(&server, HOUR, HOUR);
    let c = v
        .validate(&sign(KEY1_PEM, Some("k1"), &claims()))
        .await
        .unwrap();
    assert_eq!(c.oid, "user-oid");
    assert_eq!(c.tenant_id.as_deref(), Some("tenant-1"));
    assert_eq!(c.name, "Ada Lovelace");
    assert_eq!(c.email, "ada@example.com");
    assert_eq!(c.groups, ["g1", "g2"]);
    assert_eq!(c.roles, ["Rag.Admin"]);
}

#[tokio::test]
async fn app_id_uri_audience_is_accepted() {
    let server = server_with_key1(1).await;
    let v = validator(&server, HOUR, HOUR);
    let mut c = claims();
    c["aud"] = json!(format!("api://{AUDIENCE}"));
    v.validate(&sign(KEY1_PEM, Some("k1"), &c)).await.unwrap();
}

#[tokio::test]
async fn bad_claims_are_rejected() {
    let server = server_with_key1(1).await;
    let v = validator(&server, HOUR, HOUR);

    let check = |mutate: fn(&mut Value)| {
        let mut c = claims();
        mutate(&mut c);
        sign(KEY1_PEM, Some("k1"), &c)
    };

    let wrong_iss = check(|c| c["iss"] = json!("https://login.microsoftonline.com/other/v2.0"));
    assert!(matches!(
        v.validate(&wrong_iss).await,
        Err(TokenError::InvalidIssuer)
    ));

    let wrong_aud = check(|c| c["aud"] = json!("someone-else"));
    assert!(matches!(
        v.validate(&wrong_aud).await,
        Err(TokenError::InvalidAudience)
    ));

    let expired = check(|c| c["exp"] = json!(now() - 120));
    assert!(matches!(
        v.validate(&expired).await,
        Err(TokenError::Expired)
    ));

    let not_yet = check(|c| c["nbf"] = json!(now() + 120));
    assert!(matches!(
        v.validate(&not_yet).await,
        Err(TokenError::NotYetValid)
    ));

    let no_exp = check(|c| {
        c.as_object_mut().unwrap().remove("exp");
    });
    assert!(matches!(
        v.validate(&no_exp).await,
        Err(TokenError::InvalidClaims(_))
    ));

    let no_aud = check(|c| {
        c.as_object_mut().unwrap().remove("aud");
    });
    assert!(v.validate(&no_aud).await.is_err());

    let no_oid = check(|c| {
        let o = c.as_object_mut().unwrap();
        o.remove("oid");
        o.remove("sub");
    });
    assert!(matches!(
        v.validate(&no_oid).await,
        Err(TokenError::InvalidClaims(_))
    ));
}

#[tokio::test]
async fn small_clock_skew_is_tolerated() {
    let server = server_with_key1(1).await;
    let v = validator(&server, HOUR, HOUR);
    let mut c = claims();
    c["exp"] = json!(now() - 20);
    c["nbf"] = json!(now() + 20);
    v.validate(&sign(KEY1_PEM, Some("k1"), &c)).await.unwrap();
}

#[tokio::test]
async fn wrong_signature_and_bad_headers_are_rejected() {
    let server = server_with_key1(1).await;
    let v = validator(&server, HOUR, HOUR);

    // Signed with key 2 but claiming key 1's kid.
    let forged = sign(KEY2_PEM, Some("k1"), &claims());
    assert!(matches!(
        v.validate(&forged).await,
        Err(TokenError::InvalidSignature)
    ));

    // Tampered payload.
    let good = sign(KEY1_PEM, Some("k1"), &claims());
    let mut parts: Vec<&str> = good.split('.').collect();
    let other = sign(KEY1_PEM, Some("k1"), &json!({"oid": "evil"}));
    parts[1] = other.split('.').nth(1).unwrap();
    assert!(matches!(
        v.validate(&parts.join(".")).await,
        Err(TokenError::InvalidSignature)
    ));

    let missing_kid = sign(KEY1_PEM, None, &claims());
    assert!(matches!(
        v.validate(&missing_kid).await,
        Err(TokenError::MissingKid)
    ));

    let mut hs = Header::new(Algorithm::HS256);
    hs.kid = Some("k1".to_owned());
    let hs_token =
        jsonwebtoken::encode(&hs, &claims(), &EncodingKey::from_secret(b"shared")).unwrap();
    assert!(matches!(
        v.validate(&hs_token).await,
        Err(TokenError::UnsupportedAlgorithm)
    ));

    assert!(matches!(
        v.validate("not-a-jwt").await,
        Err(TokenError::Malformed)
    ));
}

#[tokio::test]
async fn jwks_is_cached_across_validations() {
    let server = server_with_key1(1).await;
    let v = validator(&server, HOUR, Duration::ZERO);
    for _ in 0..5 {
        v.validate(&sign(KEY1_PEM, Some("k1"), &claims()))
            .await
            .unwrap();
    }
    // `expect(1)` is verified when the server drops.
}

#[tokio::test]
async fn unknown_kid_triggers_refetch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(jwks(&[jwk(KEY1_PUB, "k1")]))
        .up_to_n_times(1)
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(jwks(&[jwk(KEY1_PUB, "k1"), jwk(KEY2_PUB, "k2")]))
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;

    let v = validator(&server, HOUR, Duration::ZERO);
    v.validate(&sign(KEY1_PEM, Some("k1"), &claims()))
        .await
        .unwrap();
    // Key rotated in: the cached set lacks k2, so it is refetched once.
    v.validate(&sign(KEY2_PEM, Some("k2"), &claims()))
        .await
        .unwrap();
    // Both keys now cached.
    v.validate(&sign(KEY1_PEM, Some("k1"), &claims()))
        .await
        .unwrap();
    v.validate(&sign(KEY2_PEM, Some("k2"), &claims()))
        .await
        .unwrap();
}

#[tokio::test]
async fn unknown_kid_refetch_is_rate_limited() {
    let server = server_with_key1(1).await;
    let v = validator(&server, HOUR, HOUR);
    v.validate(&sign(KEY1_PEM, Some("k1"), &claims()))
        .await
        .unwrap();
    for kid in ["bogus-1", "bogus-2", "bogus-3"] {
        assert!(matches!(
            v.validate(&sign(KEY1_PEM, Some(kid), &claims())).await,
            Err(TokenError::UnknownKey)
        ));
    }
}

#[tokio::test]
async fn unknown_kid_after_refetch_is_rejected() {
    let server = server_with_key1(2).await;
    let v = validator(&server, HOUR, Duration::ZERO);
    v.validate(&sign(KEY1_PEM, Some("k1"), &claims()))
        .await
        .unwrap();
    assert!(matches!(
        v.validate(&sign(KEY2_PEM, Some("k2"), &claims())).await,
        Err(TokenError::UnknownKey)
    ));
}

#[tokio::test]
async fn expired_cache_is_refetched() {
    let server = server_with_key1(3).await;
    let v = validator(&server, Duration::ZERO, Duration::ZERO);
    for _ in 0..3 {
        v.validate(&sign(KEY1_PEM, Some("k1"), &claims()))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn unreachable_jwks_is_a_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let v = validator(&server, HOUR, Duration::ZERO);
    let err = v
        .validate(&sign(KEY1_PEM, Some("k1"), &claims()))
        .await
        .unwrap_err();
    assert!(matches!(err, TokenError::KeysUnavailable));
    assert!(err.is_server_error());
}

#[tokio::test]
async fn stale_keys_are_served_when_refresh_fails() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(jwks(&[jwk(KEY1_PUB, "k1")]))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(JWKS_PATH))
        .respond_with(ResponseTemplate::new(500))
        .with_priority(2)
        .mount(&server)
        .await;

    let v = validator(&server, Duration::ZERO, Duration::ZERO);
    for _ in 0..3 {
        v.validate(&sign(KEY1_PEM, Some("k1"), &claims()))
            .await
            .unwrap();
    }
}

#[test]
fn error_messages_do_not_echo_tokens() {
    for err in [
        TokenError::Malformed,
        TokenError::InvalidSignature,
        TokenError::Expired,
        TokenError::UnknownKey,
    ] {
        assert!(!err.to_string().contains("eyJ"));
        assert!(!err.is_server_error());
    }
}
