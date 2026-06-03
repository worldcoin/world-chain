use core::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// Declare the closed set of acceptance manifest features.
///
/// Each `Variant => "key"` pair binds an enum variant to its stable snake_case
/// manifest key. From a single variant list the macro generates the enum and
/// the boilerplate the manifest and catalog rely on: [`ALL`](Feature::ALL), the
/// key (`as_str`/`Display`), and a case-insensitive [`FromStr`] that mirrors the
/// manifest's key normalization.
macro_rules! features {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident => $key:literal
            ),* $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant,
            )*
        }

        impl $name {
            /// Every feature, in declaration order.
            pub const ALL: [Self; [$(stringify!($variant)),*].len()] = [$(Self::$variant),*];

            /// The stable snake_case manifest key for this feature.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $key,)*
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
                match normalized.as_str() {
                    $($key => Ok(Self::$variant),)*
                    _ => Err(format!("unknown feature `{value}`")),
                }
            }
        }
    };
}

features! {
    /// Features the acceptance manifest can commit to.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Feature {
        Flashblocks => "flashblocks",
        BlockAccessList => "block_access_list",
        Pbh => "pbh",
    }
}
