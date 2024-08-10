use std::borrow::Cow;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config<'a> {
    username: Cow<'a, str>,
    token: Cow<'a, str>,
}
