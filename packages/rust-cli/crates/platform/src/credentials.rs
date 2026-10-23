// 复制自 apps/zcode-host/crates/account/src/account/credential.rs（1f0bfd1b71）的 `CredentialCipher` 与读取
// 部分。改动：文件经 rust-cli-fs 读写；密钥构造失败返回错误而不是 panic；密钥材料（平台、home、用户名）
// 由入口给出，不在这里读进程环境。写入取 Node `shared-credentials.ts` 的 `mutateRawCredentialRecord`
// （33a9541140）：目录锁内读改写，原子写回（platform.md §5.5）。
//! 共享凭据 `credentials.json`（docs/specs/rust-cli/platform.md §5.1）。
//!
//! 与 Node Host、Node CLI 共用：值用 AES-256-GCM 加密成 `enc:v1:<iv>.<tag>.<ciphertext>`（无填充 base64url），
//! 密钥是 `sha256(secret)`，secret 是 `ZCODE_CREDENTIAL_SECRET`，没有时是
//! `zcode-credential-fallback:<platform>:<homedir>:<username>`（Node `resolveCredentialSecret`）。
//! 不缓存文件内容：每次读取都以磁盘为准。写入在与 Node 同形的目录锁内读改写（[`CredentialStore::mutate`]）。

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use rust_cli_base::secret::RedactedString;
use rust_cli_fs::{FileMode, LockOptions, ParentDir, TempName, WriteOptions};
use rust_cli_paths::StoragePath;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const ENCRYPTED_VALUE_PREFIX: &str = "enc:v1:";
const TAG_BYTES: usize = 16;

/// Node `atomicWritePrivateTextFile`：隐藏临时文件、0600、rename 被占用时的重试间隔。
pub(crate) const PRIVATE_WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Private,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: rust_cli_fs::NODE_RENAME_RETRY,
};

/// Node `Buffer.from(raw, "base64url")` 接受有无填充两种写法；输出始终无填充。
const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// 解密失败（原因文案与 Node 相同：`凭据解密失败：<原因>`）。
    #[error("凭据解密失败：{0}")]
    Decrypt(&'static str),
    #[error("ZCode credentials are corrupt: {0}")]
    Corrupt(String),
    #[error("unable to read ZCode credentials: {0}")]
    Io(String),
    #[error("cannot encrypt the credential: {0}")]
    Encrypt(&'static str),
    #[error("unable to write ZCode credentials: {0}")]
    Write(String),
}

/// 生成凭据密钥的材料（入口在启动时读好传入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSecret {
    /// `ZCODE_CREDENTIAL_SECRET`（已去掉首尾空白，空值视为没有）。
    pub explicit: Option<RedactedString>,
    /// Node `os.platform()`：darwin / linux / win32。
    pub platform: String,
    /// Node `os.homedir()`。
    pub home: String,
    /// Node `os.userInfo().username`。
    pub username: String,
}

impl CredentialSecret {
    /// Node `defaultCredentialSecret`。
    fn derive(&self) -> String {
        match &self.explicit {
            Some(secret) if !secret.expose().trim().is_empty() => secret.expose().trim().to_owned(),
            _ => format!(
                "zcode-credential-fallback:{}:{}:{}",
                self.platform, self.home, self.username
            ),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Cipher {
    key: [u8; 32],
}

impl Cipher {
    pub fn new(secret: &CredentialSecret) -> Self {
        Self {
            key: Sha256::digest(secret.derive().as_bytes()).into(),
        }
    }

    fn sealing_key(&self) -> Result<LessSafeKey, CredentialError> {
        UnboundKey::new(&AES_256_GCM, &self.key)
            .map(LessSafeKey::new)
            .map_err(|_| CredentialError::Encrypt("AES-256 key length"))
    }

    /// 用随机 IV 加密（Node `randomBytes(12)`）。
    #[expect(
        clippy::disallowed_methods,
        reason = "platform 是加密强度随机数的入口：凭据的 IV（README §4 的随机类别，F58）"
    )]
    pub fn encrypt(&self, value: &str) -> Result<String, CredentialError> {
        let mut iv = [0u8; NONCE_LEN];
        SystemRandom::new()
            .fill(&mut iv)
            .map_err(|_| CredentialError::Encrypt("random IV"))?;
        self.encrypt_with_iv(value, iv)
    }

    /// 指定 IV 的加密：测试用它证明密文格式与 Node 逐字节相同。
    pub fn encrypt_with_iv(
        &self,
        value: &str,
        iv: [u8; NONCE_LEN],
    ) -> Result<String, CredentialError> {
        let mut data = value.as_bytes().to_vec();
        let tag = self
            .sealing_key()?
            .seal_in_place_separate_tag(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut data)
            .map_err(|_| CredentialError::Encrypt("AES-GCM seal"))?;
        Ok(format!(
            "{ENCRYPTED_VALUE_PREFIX}{}.{}.{}",
            BASE64URL.encode(iv),
            BASE64URL.encode(tag.as_ref()),
            BASE64URL.encode(&data)
        ))
    }

    /// 解密；没有 `enc:v1:` 前缀的旧值原样返回（Node 相同）。
    pub fn decrypt(&self, value: &str) -> Result<String, CredentialError> {
        let Some(payload) = value.strip_prefix(ENCRYPTED_VALUE_PREFIX) else {
            return Ok(value.to_owned());
        };
        let parts: Vec<&str> = payload.split('.').collect();
        if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
            return Err(CredentialError::Decrypt("密文格式非法"));
        }
        let decode = |raw: &str| BASE64URL.decode(raw).unwrap_or_default();
        let (iv, tag, cipher) = (decode(parts[0]), decode(parts[1]), decode(parts[2]));
        let iv: [u8; NONCE_LEN] = iv
            .try_into()
            .map_err(|_| CredentialError::Decrypt("IV 长度非法"))?;
        if tag.len() != TAG_BYTES {
            return Err(CredentialError::Decrypt("AuthTag 长度非法"));
        }
        let mut data = cipher;
        data.extend_from_slice(&tag);
        let key = self
            .sealing_key()
            .map_err(|_| CredentialError::Decrypt("密钥不匹配或密文已损坏"))?;
        let plain = key
            .open_in_place(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut data)
            .map_err(|_| CredentialError::Decrypt("密钥不匹配或密文已损坏"))?;
        Ok(String::from_utf8_lossy(plain).into_owned())
    }
}

/// 一次读取的凭据记录：值按需解密。
pub(crate) struct Credentials {
    record: Map<String, Value>,
    cipher: Cipher,
}

impl Credentials {
    /// 一个键的明文；没有这个键时为空。
    pub fn get(&self, key: &str) -> Result<Option<RedactedString>, CredentialError> {
        match self.record.get(key).and_then(Value::as_str) {
            None => Ok(None),
            Some(raw) => self
                .cipher
                .decrypt(raw)
                .map(|plain| Some(RedactedString::new(plain))),
        }
    }

    /// 写入一个键：值加密后存放（Node `save`）。
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), CredentialError> {
        let encrypted = self.cipher.encrypt(value)?;
        self.record.insert(key.to_owned(), Value::String(encrypted));
        Ok(())
    }

    /// 删掉一个键（不需要解密）。
    pub fn remove(&mut self, key: &str) {
        self.record.shift_remove(key);
    }

    /// 明文去掉首尾空白后非空。解密失败按没有处理（Node：坏密文不能当作已登录）。
    pub fn present(&self, key: &str) -> Option<RedactedString> {
        match self.get(key) {
            Ok(Some(value)) if !value.expose().trim().is_empty() => {
                Some(RedactedString::new(value.expose().trim()))
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "ignoring an undecryptable credential");
                None
            }
        }
    }
}

/// `credentials.json` 的读取方。
#[derive(Clone)]
pub(crate) struct CredentialStore {
    file: StoragePath,
    cipher: Cipher,
}

impl CredentialStore {
    pub fn new(file: StoragePath, secret: &CredentialSecret) -> Self {
        Self {
            file,
            cipher: Cipher::new(secret),
        }
    }

    pub fn file(&self) -> &StoragePath {
        &self.file
    }

    /// 在目录锁内读改写（Node `mutateRawCredentialRecord`）：持锁读文件，`change` 改记录，原子写回
    /// `JSON.stringify(record, null, 2)` 加换行。文件损坏时不写（不能把别的进程的凭据全部抹掉）；`change`
    /// 失败时也不写。
    pub async fn mutate<T>(
        &self,
        change: impl FnOnce(&mut Credentials) -> Result<T, CredentialError>,
    ) -> Result<T, CredentialError> {
        let file = self.file.as_path();
        rust_cli_fs::with_file_lock(file, &LockOptions::default(), async || {
            let mut credentials = self.read().await?;
            let result = change(&mut credentials)?;
            let mut text = serde_json::to_string_pretty(&credentials.record)
                .map_err(|error| CredentialError::Write(error.to_string()))?;
            text.push('\n');
            rust_cli_fs::atomic_write(file, text.as_bytes(), PRIVATE_WRITE)
                .await
                .map_err(|error| CredentialError::Write(format!("{}: {error}", file.display())))?;
            Ok(result)
        })
        .await
    }

    /// 读整个文件。不存在时是空记录；不是“值全为字符串的对象”时报损坏（Node 拒绝把它当空 store）。
    pub async fn read(&self) -> Result<Credentials, CredentialError> {
        let bytes = rust_cli_fs::read_optional(self.file.as_path())
            .await
            .map_err(|error| {
                CredentialError::Io(format!("{}: {error}", self.file.as_path().display()))
            })?;
        let record = match bytes {
            None => Map::new(),
            Some(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(Value::Object(record)) if record.values().all(Value::is_string) => record,
                _ => {
                    return Err(CredentialError::Corrupt(
                        self.file.as_path().display().to_string(),
                    ));
                }
            },
        };
        Ok(Credentials {
            record,
            cipher: self.cipher.clone(),
        })
    }
}

impl From<std::io::Error> for CredentialError {
    /// 目录锁的错误（超时、权限）。
    fn from(error: std::io::Error) -> Self {
        Self::Write(error.to_string())
    }
}

#[cfg(test)]
#[path = "credentials_tests.rs"]
mod tests;
