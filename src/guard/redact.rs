//! Secret redaction for tokens and free-form text (log lines, error messages).
//!
//! * [`token`] masks a single confirmation token, keeping at most 8 characters
//!   of the secret before appending `***`.
//! * [`text`] redacts `ctk_` tokens and bearer credentials embedded in text,
//!   and truncates very long text so full bodies are never reproduced in logs.

/// Maximum characters emitted by [`text`] before truncation.
const TEXT_LIMIT: usize = 1024;

/// Number of secret characters kept by [`token`] before masking.
const TOKEN_KEEP: usize = 8;

/// Mask a confirmation token: `ctk_1234567890` becomes `ctk_12345678***`.
/// Tokens with at most 8 secret characters are returned unchanged.
pub fn token(s: &str) -> String {
    const PREFIX: &str = "ctk_";

    let body = s.strip_prefix(PREFIX).unwrap_or(s);
    let prefix = if s.starts_with(PREFIX) { PREFIX } else { "" };
    let shown: String = body.chars().take(TOKEN_KEEP).collect();

    if body.chars().count() > TOKEN_KEEP {
        format!("{prefix}{shown}***")
    } else {
        format!("{prefix}{shown}")
    }
}

/// Redact secrets in free-form text and prevent bodies from being logged in
/// full: masks `ctk_` tokens and `Bearer` credentials, and truncates to
/// [`TEXT_LIMIT`] characters.
pub fn text(s: &str) -> String {
    let redacted = redact_bearers(&redact_ctk_tokens(s));
    let count = redacted.chars().count();
    if count <= TEXT_LIMIT {
        redacted
    } else {
        let mut out: String = redacted.chars().take(TEXT_LIMIT).collect();
        out.push('…');
        out
    }
}

/// Replace every `ctk_<token>` run with its masked form.
fn redact_ctk_tokens(input: &str) -> String {
    const PREFIX: &str = "ctk_";

    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find(PREFIX) {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + PREFIX.len()..];
        let token_len = after.find(char::is_whitespace).unwrap_or(after.len());
        let token_str = &rest[idx..idx + PREFIX.len() + token_len];
        out.push_str(&token(token_str));
        rest = &rest[idx + PREFIX.len() + token_len..];
    }
    out.push_str(rest);
    out
}

/// Replace every `Bearer <credential>` (case-insensitive) with
/// `Bearer ***`.
fn redact_bearers(input: &str) -> String {
    const WORD: &str = "bearer";

    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let tail = &input[i..];
        let mut matched = false;
        if tail.len() >= WORD.len() {
            let head = &tail.as_bytes()[..WORD.len()];
            if head.iter().all(u8::is_ascii) && tail[..WORD.len()].eq_ignore_ascii_case(WORD) {
                matched = true;
            }
        }

        if matched {
            let mut k = i + WORD.len();
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            let ws_end = k;
            // Require whitespace after "bearer" so words like "bearerish" are
            // left alone.
            if ws_end > i + WORD.len() {
                while k < bytes.len() && !bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                if k > ws_end {
                    out.push_str(&input[i..i + WORD.len()]);
                    out.push_str(&input[i + WORD.len()..ws_end]);
                    out.push_str("***");
                    i = k;
                    continue;
                }
            }
        }

        let ch = input[i..]
            .chars()
            .next()
            .expect("indexed into a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
