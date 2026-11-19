//! serde helpers that reproduce how the C# app reads and writes `state.json`
//! (System.Text.Json defaults: PascalCase keys, enums as numbers, nulls omitted on write).

use serde::{Deserialize, Deserializer};

/// `null` reads as the type's default. In C# an explicit `null` for a reference-typed property
/// is assigned as-is and blows up on first use; reading it as empty is the safe equivalent.
pub(crate) fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Declares an enum that System.Text.Json (no `JsonStringEnumConverter`) stores as a number.
///
/// Reading is more lenient than C#: a variant name is accepted as well as its number, and an
/// undefined number falls back to the default variant instead of being carried around as an
/// unnamed value (every C# `switch` on these enums has a default arm that behaves like it).
macro_rules! numeric_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident { $($(#[$vmeta:meta])* $variant:ident = $value:literal),+ $(,)? }
        default = $default:ident;
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $name { $($(#[$vmeta])* $variant),+ }

        impl Default for $name {
            fn default() -> Self { Self::$default }
        }

        impl $name {
            /// The number System.Text.Json writes for this variant.
            pub const fn number(self) -> i64 {
                match self { $(Self::$variant => $value),+ }
            }

            /// The variant for a stored number, if it is one of the defined values.
            pub const fn from_number(n: i64) -> Option<Self> {
                match n { $($value => Some(Self::$variant),)+ _ => None }
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_i64(self.number())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                #[derive(serde::Deserialize)]
                #[serde(untagged)]
                enum Raw { Num(i64), Text(String) }
                Ok(match Option::<Raw>::deserialize(d)? {
                    Some(Raw::Num(n)) => Self::from_number(n).unwrap_or_default(),
                    Some(Raw::Text(t)) => match t.as_str() {
                        $(stringify!($variant) => Self::$variant,)+
                        _ => Self::default(),
                    },
                    None => Self::default(),
                })
            }
        }
    };
}

pub(crate) use numeric_enum;

/// Writes a double the way System.Text.Json does: a whole number has no fractional part
/// (`1000`, not `1000.0`). Both read back identically; this keeps a rewritten `state.json`
/// textually close to the one the C# app wrote.
pub(crate) fn ser_f64<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15 {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

/// [`ser_f64`] for an optional field (always paired with `skip_serializing_if = "Option::is_none"`).
pub(crate) fn ser_opt_f64<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(v) => ser_f64(v, s),
        None => s.serialize_none(),
    }
}
