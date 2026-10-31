//! Unix 文件权限位。

/// 只有本用户能读写的文件。
pub const PRIVATE_FILE: u32 = 0o600;
/// 只有本用户能进入的目录。
pub const PRIVATE_DIR: u32 = 0o700;
/// 可执行文件（本用户可写，所有人可读、可执行）。
pub const EXECUTABLE: u32 = 0o755;
/// `mode` 里的权限位（去掉文件类型与 setuid 等位）。
pub const PERMISSION_BITS: u32 = 0o777;
