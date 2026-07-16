use crate::session::SessionRuntime;
use omini_protocol as client_proto;

impl SessionRuntime {
    pub async fn register_client_connection(&self, client_id: String) -> Option<String> {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .register(client_id);
        if changed {
            let _ = self.controller_tx.send(controller_id.clone());
        }
        controller_id
    }

    pub async fn unregister_client_connection(&self, client_id: &str) {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .unregister(client_id);
        if changed {
            let _ = self.controller_tx.send(controller_id);
        }
    }

    pub async fn claim_controller(&self, client_id: String) -> Option<String> {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .claim(client_id)?;
        if changed {
            let _ = self.controller_tx.send(controller_id.clone());
        }
        controller_id
    }

    pub async fn takeover_controller(&self, client_id: String) -> Option<String> {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .takeover(client_id)?;
        if changed {
            let _ = self.controller_tx.send(controller_id.clone());
        }
        controller_id
    }

    pub async fn release_controller(&self, client_id: &str) {
        let (controller_id, changed) = self
            .presence
            .lock()
            .expect("presence lock poisoned")
            .release(client_id);
        if changed {
            let _ = self.controller_tx.send(controller_id);
        }
    }

    pub async fn is_controller(&self, client_id: &str) -> bool {
        let presence = self.presence.lock().expect("presence lock poisoned");
        presence.controller_id.as_deref() == Some(client_id)
            && presence.clients.contains_key(client_id)
    }

    pub async fn controller_id(&self) -> Option<String> {
        self.presence
            .lock()
            .expect("presence lock poisoned")
            .controller_id
            .clone()
    }

    pub async fn is_client_connected(&self, client_id: &str) -> bool {
        self.presence
            .lock()
            .expect("presence lock poisoned")
            .clients
            .contains_key(client_id)
    }

    pub(crate) fn has_connected_clients(&self) -> bool {
        let presence = self.presence.lock().expect("presence lock poisoned");
        !presence.clients.is_empty()
    }

    pub async fn client_role(&self, client_id: &str) -> client_proto::ClientSessionRole {
        if self.is_controller(client_id).await {
            client_proto::ClientSessionRole::Controller
        } else {
            client_proto::ClientSessionRole::Observer
        }
    }
}
