use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand_core::{OsRng, RngCore};
use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

macro_rules! fixed_id {
    ($name:ident, $len:expr, $label:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; $len]);

        impl $name {
            pub const LEN: usize = $len;

            pub fn random() -> Self {
                let mut value = [0_u8; $len];
                OsRng.fill_bytes(&mut value);
                Self(value)
            }

            pub const fn from_bytes(value: [u8; $len]) -> Self {
                Self(value)
            }

            pub const fn as_bytes(&self) -> &[u8; $len] {
                &self.0
            }

            pub fn from_opaque(value: &str) -> Option<Self> {
                let decoded = URL_SAFE_NO_PAD.decode(value.as_bytes()).ok()?;
                let bytes: [u8; $len] = decoded.try_into().ok()?;
                Some(Self(bytes))
            }

            pub fn opaque(&self) -> String {
                URL_SAFE_NO_PAD.encode(self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!($label, "([redacted])"))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.opaque())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct FixedVisitor;
                impl<'de> Visitor<'de> for FixedVisitor {
                    type Value = $name;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str(concat!("opaque ", $label, " identifier"))
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: DeError,
                    {
                        $name::from_opaque(value)
                            .ok_or_else(|| E::custom("invalid opaque identifier"))
                    }
                }
                deserializer.deserialize_str(FixedVisitor)
            }
        }
    };
}

fixed_id!(RouteId, 16, "RouteId");
fixed_id!(DeviceSlotId, 16, "DeviceSlotId");
fixed_id!(OperationId, 16, "OperationId");
fixed_id!(AccessToken, 32, "AccessToken");
fixed_id!(RouteAdminToken, 32, "RouteAdminToken");

/// The persistent host iroh endpoint identity.  It is public addressing data,
/// not a business identity or a credential.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostEndpointId([u8; 32]);

impl HostEndpointId {
    pub const LEN: usize = 32;

    pub const fn from_bytes(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn from_opaque(value: &str) -> Option<Self> {
        let decoded = URL_SAFE_NO_PAD.decode(value.as_bytes()).ok()?;
        Some(Self(decoded.try_into().ok()?))
    }

    pub fn opaque(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }
}

impl fmt::Debug for HostEndpointId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HostEndpointId([redacted])")
    }
}

impl Serialize for HostEndpointId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.opaque())
    }
}

impl<'de> Deserialize<'de> for HostEndpointId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HostVisitor;
        impl<'de> Visitor<'de> for HostVisitor {
            type Value = HostEndpointId;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("opaque host endpoint identifier")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: DeError,
            {
                HostEndpointId::from_opaque(value)
                    .ok_or_else(|| E::custom("invalid host endpoint identifier"))
            }
        }
        deserializer.deserialize_str(HostVisitor)
    }
}

pub(crate) fn digest(value: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hasher.finalize().into()
}

pub(crate) fn token_digest(token: &AccessToken) -> [u8; 32] {
    digest(token.as_bytes())
}

pub(crate) fn admin_digest(token: &RouteAdminToken) -> [u8; 32] {
    digest(token.as_bytes())
}
