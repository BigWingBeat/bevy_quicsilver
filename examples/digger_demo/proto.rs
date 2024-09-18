use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct ClientHello {
    pub username: String,
    pub password: String,
}
