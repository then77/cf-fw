#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    Starting,
    Failed,
    Running,
    IdleCountdown,
    ShuttingDown,
    Exited,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteState {
    Active,
    StopRequested,
    Removed,
}
