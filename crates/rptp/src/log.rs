pub trait Log {
    fn message_received(&self, msg: &str);
}

pub struct NoopLog;

impl Log for NoopLog {
    fn message_received(&self, _msg: &str) {}
}
