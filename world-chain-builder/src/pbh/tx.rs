use strum::EnumString;
use strum_macros::Display;

#[derive(Display, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum Prefix {
    Placeholder,
}
