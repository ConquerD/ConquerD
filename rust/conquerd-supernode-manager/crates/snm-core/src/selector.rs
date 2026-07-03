use std::fmt;

#[derive(Debug, Clone, Default)]
pub struct Selector {
    pub host: Option<String>,
    pub instance: Option<String>,
    pub all: bool,
}

impl Selector {
    pub fn from_flags(host: Option<String>, instance: Option<String>, all: bool) -> Self {
        Self {
            host,
            instance,
            all,
        }
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.all {
            return write!(f, "all");
        }
        match (&self.host, &self.instance) {
            (Some(h), Some(i)) => write!(f, "{h}/{i}"),
            (Some(h), None) => write!(f, "{h}/*"),
            (None, Some(i)) => write!(f, "*/{i}"),
            (None, None) => write!(f, "(default)"),
        }
    }
}
