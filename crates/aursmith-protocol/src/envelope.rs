use ciborium::{de::from_reader, ser::into_writer};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEnvelope {
    pub schema_major: u16,
    pub schema_minor: u16,
    pub payload_type: String,
    pub payload: Vec<u8>,
    pub payload_sha256: String,
    pub verifying_key: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum EnvelopeError {
    #[error("CBOR 编码失败: {0}")]
    Encode(String),
    #[error("CBOR 解码失败: {0}")]
    Decode(String),
    #[error("payload 摘要不匹配")]
    DigestMismatch,
    #[error("Ed25519 公钥格式无效")]
    InvalidVerifyingKey,
    #[error("Ed25519 签名格式无效")]
    InvalidSignature,
    #[error("签名验证失败")]
    VerificationFailed,
    #[error("payload 类型不匹配，期望 {expected}，实际 {actual}")]
    PayloadTypeMismatch { expected: String, actual: String },
}

impl SignedEnvelope {
    pub fn sign<T: Serialize>(
        payload_type: impl Into<String>,
        payload: &T,
        signing_key: &SigningKey,
    ) -> Result<Self, EnvelopeError> {
        let mut bytes = Vec::new();
        into_writer(payload, &mut bytes)
            .map_err(|error| EnvelopeError::Encode(error.to_string()))?;
        let digest = Sha256::digest(&bytes);
        let signature = signing_key.sign(&bytes);
        Ok(Self {
            schema_major: crate::PROTOCOL_MAJOR,
            schema_minor: crate::PROTOCOL_MINOR,
            payload_type: payload_type.into(),
            payload: bytes,
            payload_sha256: hex::encode(digest),
            verifying_key: signing_key.verifying_key().to_bytes().to_vec(),
            signature: signature.to_bytes().to_vec(),
        })
    }

    pub fn verify<T: DeserializeOwned>(&self, expected_type: &str) -> Result<T, EnvelopeError> {
        if self.payload_type != expected_type {
            return Err(EnvelopeError::PayloadTypeMismatch {
                expected: expected_type.to_owned(),
                actual: self.payload_type.clone(),
            });
        }
        let actual_digest = hex::encode(Sha256::digest(&self.payload));
        if actual_digest != self.payload_sha256 {
            return Err(EnvelopeError::DigestMismatch);
        }
        let key_bytes: [u8; 32] = self
            .verifying_key
            .as_slice()
            .try_into()
            .map_err(|_| EnvelopeError::InvalidVerifyingKey)?;
        let signature_bytes: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| EnvelopeError::InvalidSignature)?;
        let key =
            VerifyingKey::from_bytes(&key_bytes).map_err(|_| EnvelopeError::InvalidVerifyingKey)?;
        let signature = Signature::from_bytes(&signature_bytes);
        key.verify(&self.payload, &signature)
            .map_err(|_| EnvelopeError::VerificationFailed)?;
        from_reader(self.payload.as_slice())
            .map_err(|error| EnvelopeError::Decode(error.to_string()))
    }

    pub fn verifying_key(&self) -> Result<VerifyingKey, EnvelopeError> {
        let bytes: [u8; 32] = self
            .verifying_key
            .as_slice()
            .try_into()
            .map_err(|_| EnvelopeError::InvalidVerifyingKey)?;
        VerifyingKey::from_bytes(&bytes).map_err(|_| EnvelopeError::InvalidVerifyingKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Message {
        value: String,
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7_u8; 32])
    }

    #[test]
    fn signed_payload_round_trips() {
        let message = Message {
            value: "不可变输入".to_owned(),
        };
        let envelope = SignedEnvelope::sign("test.message", &message, &signing_key()).unwrap();
        assert_eq!(envelope.verify::<Message>("test.message").unwrap(), message);
    }

    #[test]
    fn tampering_is_rejected_before_decode() {
        let mut envelope = SignedEnvelope::sign(
            "test.message",
            &Message {
                value: "原始".into(),
            },
            &signing_key(),
        )
        .unwrap();
        envelope.payload.push(0);
        assert!(matches!(
            envelope.verify::<Message>("test.message"),
            Err(EnvelopeError::DigestMismatch)
        ));
    }

    #[test]
    fn payload_type_is_part_of_the_contract() {
        let envelope = SignedEnvelope::sign(
            "test.message",
            &Message {
                value: "原始".into(),
            },
            &signing_key(),
        )
        .unwrap();
        assert!(matches!(
            envelope.verify::<Message>("other.message"),
            Err(EnvelopeError::PayloadTypeMismatch { .. })
        ));
    }
}
