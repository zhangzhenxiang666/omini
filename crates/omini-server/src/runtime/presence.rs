use super::*;

#[derive(Debug, Default)]
pub(super) struct ClientPresence {
    // 同一 client_id 可能打开多个 WebSocket，计数归零才算真正离线。
    pub clients: HashMap<String, usize>,
    // controller 永远只能是在线客户端；释放/断开时会自动转给其它在线客户端。
    pub controller_id: Option<String>,
}

impl ClientPresence {
    pub(super) fn register(&mut self, client_id: String) -> (Option<String>, bool) {
        let before = self.controller_id.clone();
        *self.clients.entry(client_id.clone()).or_insert(0) += 1;
        if self.controller_id.is_none() {
            self.controller_id = Some(client_id);
        }
        let after = self.controller_id.clone();
        (after.clone(), before != after)
    }

    pub(super) fn unregister(&mut self, client_id: &str) -> (Option<String>, bool) {
        let before = self.controller_id.clone();
        if let Some(count) = self.clients.get_mut(client_id) {
            if *count > 1 {
                *count -= 1;
                return (self.controller_id.clone(), false);
            }
            self.clients.remove(client_id);
        }

        if before.as_deref() == Some(client_id) {
            self.controller_id = self.random_client_id(None);
        }
        let after = self.controller_id.clone();
        (after.clone(), before != after)
    }

    pub(super) fn claim(&mut self, client_id: String) -> Option<(Option<String>, bool)> {
        if !self.clients.contains_key(&client_id) {
            return None;
        }
        let before = self.controller_id.clone();
        // claim 只在当前没有 controller 时生效，避免观察者无意覆盖已有控制者。
        if self.controller_id.is_none() {
            self.controller_id = Some(client_id);
        }
        let after = self.controller_id.clone();
        Some((after.clone(), before != after))
    }

    pub(super) fn takeover(&mut self, client_id: String) -> Option<(Option<String>, bool)> {
        if !self.clients.contains_key(&client_id) {
            return None;
        }
        let before = self.controller_id.clone();
        // takeover 是显式抢占入口，调用方必须已经确认这是用户意图或安全的自动接管。
        self.controller_id = Some(client_id);
        let after = self.controller_id.clone();
        Some((after.clone(), before != after))
    }

    pub(super) fn release(&mut self, client_id: &str) -> (Option<String>, bool) {
        let before = self.controller_id.clone();
        if before.as_deref() == Some(client_id) {
            // 释放 controller 后仍保留“有连接就有控制者”的不变量。
            self.controller_id = self.random_client_id(Some(client_id));
        }
        let after = self.controller_id.clone();
        (after.clone(), before != after)
    }

    fn random_client_id(&self, exclude: Option<&str>) -> Option<String> {
        let candidates = self
            .clients
            .keys()
            .filter(|candidate| exclude != Some(candidate.as_str()))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        Some(candidates[random_index(candidates.len())].to_string())
    }
}

fn random_index(len: usize) -> usize {
    debug_assert!(len > 0);
    let random = uuid::Uuid::new_v4();
    let mut value = 0usize;
    for byte in random.as_bytes().iter().take(std::mem::size_of::<usize>()) {
        value = (value << 8) | usize::from(*byte);
    }
    value % len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_connected_client_becomes_controller() {
        let mut presence = ClientPresence::default();

        let (controller_id, changed) = presence.register("client_1".to_string());

        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_1"));
        assert_eq!(presence.controller_id.as_deref(), Some("client_1"));
    }

    #[test]
    fn second_connected_client_observes_until_takeover() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());

        let (controller_id, changed) = presence.register("client_2".to_string());
        assert!(!changed);
        assert_eq!(controller_id.as_deref(), Some("client_1"));

        let (controller_id, changed) = presence
            .takeover("client_2".to_string())
            .expect("client_2 is connected");
        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_2"));
    }

    #[test]
    fn unconnected_client_cannot_takeover() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());

        let result = presence.takeover("client_2".to_string());

        assert!(result.is_none());
        assert_eq!(presence.controller_id.as_deref(), Some("client_1"));
        assert!(!presence.clients.contains_key("client_2"));
    }

    #[test]
    fn controller_disconnect_promotes_remaining_client() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());
        presence.register("client_2".to_string());

        let (controller_id, changed) = presence.unregister("client_1");

        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_2"));
    }

    #[test]
    fn repeated_connections_keep_client_online_until_last_disconnect() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());
        presence.register("client_1".to_string());
        presence.register("client_2".to_string());

        let (controller_id, changed) = presence.unregister("client_1");
        assert!(!changed);
        assert_eq!(controller_id.as_deref(), Some("client_1"));
        assert!(presence.clients.contains_key("client_1"));

        let (controller_id, changed) = presence.unregister("client_1");
        assert!(changed);
        assert_eq!(controller_id.as_deref(), Some("client_2"));
        assert!(!presence.clients.contains_key("client_1"));
    }

    #[test]
    fn last_disconnect_clears_controller_and_clients() {
        let mut presence = ClientPresence::default();
        presence.register("client_1".to_string());

        let (controller_id, changed) = presence.unregister("client_1");

        assert!(changed);
        assert_eq!(controller_id, None);
        assert!(presence.clients.is_empty());
    }
}
