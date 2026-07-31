use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use thiserror::Error;
use tracedecay_domain::SignedCursorKeyRefV1;
use zeroize::Zeroizing;

pub(super) const MAX_CURSOR_SECRET_BYTES: usize = 256;
const CURSOR_KEY_DERIVATION_DOMAIN_V1: &[u8] = b"tracedecay.cursor-key-derivation.v1\0";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CursorKeyError {
    #[error("cursor authentication key is unavailable")]
    Unavailable,
    #[error("cursor authentication key material is invalid")]
    InvalidMaterial,
    #[error("cursor authentication failed")]
    AuthenticationFailed,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CursorSignature([u8; 32]);

impl CursorSignature {
    pub(crate) fn from_hex(encoded: &str) -> Result<Self, CursorKeyError> {
        let decoded = hex::decode(encoded).map_err(|_| CursorKeyError::AuthenticationFailed)?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| CursorKeyError::AuthenticationFailed)?;
        Ok(Self(bytes))
    }

    pub(crate) fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

pub trait SessionCursorAuthenticator: Send + Sync {
    fn sign(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
    ) -> Result<CursorSignature, CursorKeyError>;

    fn verify(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
        signature: &CursorSignature,
    ) -> Result<(), CursorKeyError>;
}

pub struct InMemoryCursorAuthenticator {
    key: SignedCursorKeyRefV1,
    secret: Zeroizing<Vec<u8>>,
}

impl InMemoryCursorAuthenticator {
    pub fn new(
        key: SignedCursorKeyRefV1,
        secret: impl Into<Vec<u8>>,
    ) -> Result<Self, CursorKeyError> {
        let secret = Zeroizing::new(secret.into());
        if secret.len() < 32 || secret.len() > MAX_CURSOR_SECRET_BYTES {
            return Err(CursorKeyError::InvalidMaterial);
        }
        Ok(Self { key, secret })
    }

    fn mac(&self) -> Result<Hmac<Sha256>, CursorKeyError> {
        <Hmac<Sha256> as KeyInit>::new_from_slice(&self.secret)
            .map_err(|_| CursorKeyError::InvalidMaterial)
    }

    /// Derive domain-separated key material without exposing the durable
    /// cursor secret.
    pub fn derive_key_material(
        &self,
        key: &SignedCursorKeyRefV1,
        context: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CursorKeyError> {
        if key != &self.key {
            return Err(CursorKeyError::Unavailable);
        }
        let mut mac = self.mac()?;
        mac.update(CURSOR_KEY_DERIVATION_DOMAIN_V1);
        mac.update(context);
        Ok(Zeroizing::new(mac.finalize().into_bytes().to_vec()))
    }
}

impl fmt::Debug for InMemoryCursorAuthenticator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InMemoryCursorAuthenticator")
            .field("key", &self.key)
            .field("secret", &"REDACTED")
            .finish()
    }
}

impl SessionCursorAuthenticator for InMemoryCursorAuthenticator {
    fn sign(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
    ) -> Result<CursorSignature, CursorKeyError> {
        if key != &self.key {
            return Err(CursorKeyError::Unavailable);
        }
        let mut mac = self.mac()?;
        mac.update(authenticated);
        Ok(CursorSignature(mac.finalize().into_bytes().into()))
    }

    fn verify(
        &self,
        key: &SignedCursorKeyRefV1,
        authenticated: &[u8],
        signature: &CursorSignature,
    ) -> Result<(), CursorKeyError> {
        if key != &self.key {
            return Err(CursorKeyError::Unavailable);
        }
        let mut mac = self.mac()?;
        mac.update(authenticated);
        mac.verify_slice(&signature.0)
            .map_err(|_| CursorKeyError::AuthenticationFailed)
    }
}
