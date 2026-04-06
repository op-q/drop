use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

use crate::domain::session::Session;

#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<Mutex<HashMap<String, Session>>>,
}