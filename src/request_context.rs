use crate::dpapi;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};
use url::Url;
use zeroize::Zeroizing;

pub const MAX_HEADERS: usize = 64;
pub const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 48 * 1024;

const BLOCKED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "proxy-authorization",
    "range",
    "if-range",
    "accept-encoding",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
];

const PUBLIC_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "cache-control",
    "pragma",
    "user-agent",
    "dnt",
    "sec-fetch-dest",
    "sec-fetch-mode",
    "sec-fetch-site",
    "sec-fetch-user",
    "sec-gpc",
    "upgrade-insecure-requests",
    "if-none-match",
    "if-modified-since",
    "referer",
];

#[derive(Clone, Deserialize, Serialize)]
pub struct WireRequestHeader {
    pub name: String,
    pub value: String,
}

impl WireRequestHeader {
    pub fn new(name: &str, value: &str) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

impl fmt::Debug for WireRequestHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireRequestHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct WireRequestContext {
    pub headers: Vec<WireRequestHeader>,
    pub source_page_url: Option<String>,
    pub initial_url: String,
    pub final_url: String,
    #[serde(default)]
    pub incognito: bool,
    #[serde(default)]
    pub cookie_store_id: Option<String>,
}

impl fmt::Debug for WireRequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WireRequestContext")
            .field("header_count", &self.headers.len())
            .field(
                "source_origin",
                &debug_origin(self.source_page_url.as_deref()),
            )
            .field("initial_origin", &debug_origin(Some(&self.initial_url)))
            .field("final_origin", &debug_origin(Some(&self.final_url)))
            .field("incognito", &self.incognito)
            .field("cookie_store_id", &self.cookie_store_id)
            .finish()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PublicRequestContext {
    pub headers: Vec<WireRequestHeader>,
    pub source_page_url: Option<String>,
    pub initial_url: String,
    pub final_url: String,
    #[serde(default)]
    pub incognito: bool,
    #[serde(default)]
    pub cookie_store_id: Option<String>,
}

impl fmt::Debug for PublicRequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicRequestContext")
            .field("header_count", &self.headers.len())
            .field(
                "source_origin",
                &debug_origin(self.source_page_url.as_deref()),
            )
            .field("initial_origin", &debug_origin(Some(&self.initial_url)))
            .field("final_origin", &debug_origin(Some(&self.final_url)))
            .field("incognito", &self.incognito)
            .field("cookie_store_id", &self.cookie_store_id)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SourceAuthorization {
    #[default]
    Public,
    Encrypted,
    NeedsFirefox,
    DecryptionFailed,
    ProtectedCleared,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct StoredRequestContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<PublicRequestContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted: Option<Vec<u8>>,
    #[serde(default)]
    pub was_protected: bool,
}

impl fmt::Debug for StoredRequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredRequestContext")
            .field("has_public", &self.public.is_some())
            .field("encrypted_bytes", &self.encrypted.as_ref().map(Vec::len))
            .field("was_protected", &self.was_protected)
            .finish()
    }
}

#[derive(Clone)]
pub struct RequestHeader {
    name: String,
    value: Zeroizing<String>,
}

impl fmt::Debug for RequestHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct RequestContext {
    headers: Vec<RequestHeader>,
    source_page_url: Option<Zeroizing<String>>,
    initial_url: String,
    final_url: String,
    incognito: bool,
    cookie_store_id: Option<String>,
}

impl fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestContext")
            .field("header_count", &self.headers.len())
            .field("source_origin", &debug_origin(self.source_page_url()))
            .field("initial_origin", &debug_origin(Some(&self.initial_url)))
            .field("final_origin", &debug_origin(Some(&self.final_url)))
            .field("incognito", &self.incognito)
            .field("cookie_store_id", &self.cookie_store_id)
            .finish()
    }
}

impl RequestContext {
    pub fn headers(&self) -> Vec<(String, String)> {
        self.headers
            .iter()
            .map(|header| (header.name.clone(), header.value.to_string()))
            .collect()
    }

    pub fn iter_headers(&self) -> impl Iterator<Item = (&str, &str)> {
        self.headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
    }

    pub fn source_page_url(&self) -> Option<&str> {
        self.source_page_url.as_ref().map(|url| url.as_str())
    }

    pub fn initial_url(&self) -> &str {
        &self.initial_url
    }

    pub fn final_url(&self) -> &str {
        &self.final_url
    }

    pub fn incognito(&self) -> bool {
        self.incognito
    }

    pub fn cookie_store_id(&self) -> Option<&str> {
        self.cookie_store_id.as_deref()
    }

    fn from_validated(validated: ValidatedContext) -> Self {
        Self {
            headers: validated
                .wire
                .headers
                .into_iter()
                .map(|header| RequestHeader {
                    name: header.name,
                    value: Zeroizing::new(header.value),
                })
                .collect(),
            source_page_url: validated.wire.source_page_url.map(Zeroizing::new),
            initial_url: validated.wire.initial_url,
            final_url: validated.wire.final_url,
            incognito: validated.wire.incognito,
            cookie_store_id: validated.wire.cookie_store_id,
        }
    }
}

#[derive(Clone)]
pub struct PreparedRequestContext {
    pub stored: StoredRequestContext,
    pub runtime: RequestContext,
    pub authorization: SourceAuthorization,
}

#[derive(Debug)]
pub enum RequestContextError {
    Invalid(String),
    Serialization(String),
    Protection(std::io::Error),
    Decryption(std::io::Error),
}

impl fmt::Display for RequestContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "invalid request context: {message}"),
            Self::Serialization(_) => write!(f, "request context serialization failed"),
            Self::Protection(_) => write!(f, "request context protection failed"),
            Self::Decryption(_) => write!(f, "request context decryption failed"),
        }
    }
}

impl std::error::Error for RequestContextError {}

struct ValidatedContext {
    wire: WireRequestContext,
    protected: bool,
}

fn debug_origin(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let Ok(url) = Url::parse(value) else {
        return "<invalid>".into();
    };
    let host = url.host_str().unwrap_or_default();
    if host.is_empty() {
        return url.scheme().to_owned();
    }
    match url.port() {
        Some(port) => format!("{}://{}:{port}", url.scheme(), host),
        None => format!("{}://{host}", url.scheme()),
    }
}

fn byte_len(value: &str) -> usize {
    value.len()
}

fn invalid(message: impl Into<String>) -> RequestContextError {
    RequestContextError::Invalid(message.into())
}

fn validate_url(value: &str, field: &str) -> Result<String, RequestContextError> {
    if value.is_empty() || value.contains(['\0', '\r', '\n']) {
        return Err(invalid(format!("{field} contains invalid characters")));
    }
    if byte_len(value) > MAX_CONTEXT_BYTES {
        return Err(invalid(format!("{field} is too large")));
    }
    let parsed = Url::parse(value).map_err(|_| invalid(format!("{field} is not a URL")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(invalid(format!("{field} must be an HTTP or HTTPS URL")));
    }
    Ok(value.to_owned())
}

fn validate_optional_url(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, RequestContextError> {
    value.map(|url| validate_url(&url, field)).transpose()
}

fn validate_header_name(name: &str) -> Result<(), RequestContextError> {
    if name.is_empty() || name != name.trim() {
        return Err(invalid("header name is empty or padded"));
    }
    if !name.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            )
    }) {
        return Err(invalid("header name is not an HTTP token"));
    }
    Ok(())
}

fn sensitive_header(name: &str, value: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "cookie" | "cookie2" | "authorization" | "proxy-authorization"
    ) {
        return true;
    }
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("credential")
        || lower.contains("session")
        || lower.contains("signature")
        || lower.contains("jwt")
    {
        return true;
    }
    if lower == "referer" {
        return Url::parse(value)
            .map(|url| url.query().is_some() || url.fragment().is_some())
            .unwrap_or(true);
    }
    !PUBLIC_HEADERS.contains(&lower.as_str())
}

fn sensitive_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return true;
    };
    let Some(query) = url.query() else {
        return false;
    };
    let sensitive_keys: HashSet<&str> = [
        "token",
        "access_token",
        "auth",
        "authorization",
        "credential",
        "expires",
        "exp",
        "key",
        "sig",
        "signature",
        "session",
        "secret",
    ]
    .into_iter()
    .collect();
    url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| {
        let lower = key.to_ascii_lowercase();
        sensitive_keys.contains(lower.as_str())
            || lower.contains("token")
            || lower.contains("signature")
            || lower.contains("secret")
    })
}

fn validate_wire(mut wire: WireRequestContext) -> Result<ValidatedContext, RequestContextError> {
    if wire.headers.len() > MAX_HEADERS {
        return Err(invalid("too many request headers"));
    }
    wire.initial_url = validate_url(&wire.initial_url, "initial URL")?;
    wire.final_url = validate_url(&wire.final_url, "final URL")?;
    wire.source_page_url = validate_optional_url(wire.source_page_url, "source page URL")?;
    if let Some(store) = &wire.cookie_store_id {
        if store.is_empty() || store.contains(['\0', '\r', '\n']) || byte_len(store) > 256 {
            return Err(invalid("cookie store id is invalid"));
        }
    }

    let mut filtered = Vec::with_capacity(wire.headers.len());
    let mut total_bytes = byte_len(&wire.initial_url) + byte_len(&wire.final_url);
    if let Some(source) = &wire.source_page_url {
        total_bytes += byte_len(source);
    }
    let mut protected = sensitive_url(&wire.initial_url) || sensitive_url(&wire.final_url);
    for mut header in wire.headers {
        validate_header_name(&header.name)?;
        if header.value.contains(['\0', '\r', '\n']) {
            return Err(invalid("header value contains invalid characters"));
        }
        if byte_len(&header.value) > MAX_HEADER_VALUE_BYTES {
            return Err(invalid("header value is too large"));
        }
        let lower = header.name.to_ascii_lowercase();
        if BLOCKED_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        protected |= sensitive_header(&header.name, &header.value);
        total_bytes += byte_len(&header.name) + byte_len(&header.value);
        if total_bytes > MAX_CONTEXT_BYTES {
            return Err(invalid("request context is too large"));
        }
        // Canonicalise only the name for stable persistence; values are kept
        // byte-for-byte so signed headers are not altered.
        header.name = header.name.trim().to_owned();
        filtered.push(header);
    }
    wire.headers = filtered;
    Ok(ValidatedContext { wire, protected })
}

pub fn prepare(wire: WireRequestContext) -> Result<PreparedRequestContext, RequestContextError> {
    let validated = validate_wire(wire)?;
    let runtime = RequestContext::from_validated(ValidatedContext {
        wire: validated.wire.clone(),
        protected: validated.protected,
    });
    if !validated.protected {
        return Ok(PreparedRequestContext {
            stored: StoredRequestContext {
                public: Some(PublicRequestContext {
                    headers: validated.wire.headers,
                    source_page_url: validated.wire.source_page_url,
                    initial_url: validated.wire.initial_url,
                    final_url: validated.wire.final_url,
                    incognito: validated.wire.incognito,
                    cookie_store_id: validated.wire.cookie_store_id,
                }),
                encrypted: None,
                was_protected: false,
            },
            runtime,
            authorization: SourceAuthorization::Public,
        });
    }
    let payload = serde_json::to_vec(&validated.wire)
        .map_err(|error| RequestContextError::Serialization(error.to_string()))?;
    let encrypted =
        dpapi::protect_current_user(&payload).map_err(RequestContextError::Protection)?;
    Ok(PreparedRequestContext {
        stored: StoredRequestContext {
            public: None,
            encrypted: Some(encrypted),
            was_protected: true,
        },
        runtime,
        authorization: SourceAuthorization::Encrypted,
    })
}

pub fn restore(
    stored: &StoredRequestContext,
) -> Result<Option<RequestContext>, RequestContextError> {
    if let Some(public) = &stored.public {
        let validated = validate_wire(WireRequestContext {
            headers: public.headers.clone(),
            source_page_url: public.source_page_url.clone(),
            initial_url: public.initial_url.clone(),
            final_url: public.final_url.clone(),
            incognito: public.incognito,
            cookie_store_id: public.cookie_store_id.clone(),
        })?;
        return Ok(Some(RequestContext::from_validated(validated)));
    }
    let Some(encrypted) = &stored.encrypted else {
        return Ok(None);
    };
    let plaintext =
        dpapi::unprotect_current_user(encrypted).map_err(RequestContextError::Decryption)?;
    let wire: WireRequestContext = serde_json::from_slice(&plaintext).map_err(|_| {
        RequestContextError::Decryption(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid protected request context",
        ))
    })?;
    Ok(Some(RequestContext::from_validated(validate_wire(wire)?)))
}

pub fn clear_secret_material(stored: &mut StoredRequestContext) {
    stored.public = None;
    stored.encrypted = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_with_headers(pairs: Vec<(&str, &str)>) -> WireRequestContext {
        WireRequestContext {
            headers: pairs
                .into_iter()
                .map(|(name, value)| WireRequestHeader::new(name, value))
                .collect(),
            source_page_url: Some("https://app.test/page".into()),
            initial_url: "https://files.test/file.pdf".into(),
            final_url: "https://files.test/file.pdf".into(),
            incognito: false,
            cookie_store_id: Some("firefox-default".into()),
        }
    }

    #[test]
    fn cookie_makes_the_entire_context_encrypted() {
        let prepared = prepare(WireRequestContext {
            headers: vec![
                WireRequestHeader::new("Accept", "application/pdf"),
                WireRequestHeader::new("Cookie", "session=secret"),
            ],
            source_page_url: Some("https://chatgpt.com/c/1".into()),
            initial_url: "https://chatgpt.com/backend-api/estuary/content?id=1".into(),
            final_url: "https://chatgpt.com/backend-api/estuary/content?id=1".into(),
            incognito: false,
            cookie_store_id: Some("firefox-default".into()),
        })
        .unwrap();
        #[cfg(windows)]
        {
            assert!(prepared.stored.public.is_none());
            assert!(prepared.stored.encrypted.is_some());
            assert_eq!(prepared.authorization, SourceAuthorization::Encrypted);
        }
        #[cfg(not(windows))]
        assert!(matches!(prepared, Err(_)));
    }

    #[test]
    fn safe_allowlist_context_stays_public() {
        let prepared = prepare(context_with_headers(vec![("Accept", "application/pdf")])).unwrap();
        assert!(prepared.stored.public.is_some());
        assert!(prepared.stored.encrypted.is_none());
        assert_eq!(prepared.authorization, SourceAuthorization::Public);
    }

    #[test]
    fn sensitive_and_unknown_headers_are_protected() {
        for (name, value) in [
            ("Authorization", "Bearer secret"),
            ("X-Download-Token", "secret"),
            ("Referer", "https://app.test/page?token=secret"),
        ] {
            #[cfg(windows)]
            {
                let prepared = prepare(context_with_headers(vec![(name, value)])).unwrap();
                assert!(prepared.stored.public.is_none(), "{name} must be protected");
                assert!(prepared.stored.encrypted.is_some());
            }
            #[cfg(not(windows))]
            assert!(prepare(context_with_headers(vec![(name, value)])).is_err());
        }
    }

    #[test]
    fn blocked_headers_are_removed_from_wire_context() {
        let prepared = prepare(context_with_headers(vec![
            ("Range", "bytes=0-10"),
            ("Accept-Encoding", "gzip"),
            ("Accept", "application/pdf"),
        ]))
        .unwrap();
        let headers = prepared.runtime.headers();
        assert_eq!(headers, vec![("Accept".into(), "application/pdf".into())]);
    }

    #[test]
    fn invalid_header_and_size_limits_fail_closed() {
        assert!(prepare(context_with_headers(vec![("X-Test", "bad\r\nvalue")])).is_err());
        let oversized = "x".repeat(8 * 1024 + 1);
        assert!(prepare(context_with_headers(vec![("X-Test", &oversized)])).is_err());
        let pairs = (0..65)
            .map(|index| (format!("X-{index}"), "v".to_owned()))
            .collect::<Vec<_>>();
        let context = WireRequestContext {
            headers: pairs
                .into_iter()
                .map(|(name, value)| WireRequestHeader { name, value })
                .collect(),
            ..context_with_headers(Vec::new())
        };
        assert!(prepare(context).is_err());
    }

    #[test]
    fn debug_output_never_contains_header_values_or_query_paths() {
        let wire = context_with_headers(vec![("Cookie", "session=super-secret")]);
        let debug = format!("{wire:?}");
        assert!(!debug.contains("super-secret"));
        assert!(!debug.contains("/page"));
    }
}
