use crate::client::ClientRequest;
use std::collections::VecDeque;

/// 更新逻辑只描述待执行操作；应用层负责网络发送和本地平台调用。
#[derive(Debug, Default)]
pub struct Effects {
    pub requests: VecDeque<ClientRequest>,
    pub clipboard: Vec<String>,
    pub local: Vec<crate::platform::files::LocalRequest>,
}

impl Effects {
    pub fn send(&mut self, request: ClientRequest) -> Result<(), std::convert::Infallible> {
        self.requests.push_back(request);
        Ok(())
    }
}
