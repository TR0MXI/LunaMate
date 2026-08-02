//! 验证配置加载、精确修改、revision 与持久化一致性。

mod agent_transaction;
mod permissions_backup;
mod revision_atomicity;
mod scalar_persistence_window;
mod startup_robustness;

use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const BOUND_AGENT_CONFIG: &str = r#"[llm]
selected = "chat"

[[llm.models]]
id = "chat"
label = "Chat"
kind = "chat-completions"
provider = "ollama"
model = "qwen3:8b"

[[llm.models]]
id = "voice"
label = "Voice"
kind = "speech-synthesis"
provider = "openai"
model = "gpt-4o-mini-tts"
api_key = "test-key"
voice = "alloy"

[persona]
selected = "moon"

[[persona.list]]
id = "moon"
name = "露娜"
model = "chat"
tts_model = "voice"
"#;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间必须晚于 Unix 纪元")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("lunamate-config-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("测试配置目录应当可以创建");
        Self(path)
    }

    fn config_path(&self) -> PathBuf {
        self.0.join("config.toml")
    }

    fn corrupt_backup_path(&self) -> PathBuf {
        self.0.join("config.toml.corrupt.bak")
    }

    fn write(&self, contents: &str) {
        fs::write(self.config_path(), contents).expect("测试配置应当可以写入");
    }

    fn write_bytes(&self, contents: &[u8]) {
        fs::write(self.config_path(), contents).expect("测试配置原始字节应当可以写入");
    }

    fn temporary_paths(&self) -> Vec<PathBuf> {
        let mut paths = fs::read_dir(&self.0)
            .expect("测试配置目录应当可以枚举")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
