pub trait PortLog {
    fn message_sent(&self, msg: &str);
    fn message_received(&self, msg: &str);
    fn state_transition(&self, from: &str, to: &str, reason: &str);
}

pub struct NoopPortLog;

impl PortLog for NoopPortLog {
    fn message_sent(&self, _msg: &str) {}
    fn message_received(&self, _msg: &str) {}
    fn state_transition(&self, _from: &str, _to: &str, _reason: &str) {}
}
