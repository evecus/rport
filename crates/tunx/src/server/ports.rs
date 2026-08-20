use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::sync::Mutex;

pub struct PortManager {
    range: (u16, u16),
    used: Mutex<BTreeSet<u16>>,
}

impl PortManager {
    pub fn new(range: (u16, u16)) -> Self {
        Self {
            range,
            used: Mutex::new(BTreeSet::new()),
        }
    }

    /// 申请端口：port=0 表示随机，否则指定端口
    pub fn acquire(&self, port: u16) -> Result<u16> {
        let mut used = self.used.lock().unwrap();
        if port == 0 {
            // 在范围内找第一个空闲端口
            for p in self.range.0..=self.range.1 {
                if !used.contains(&p) {
                    used.insert(p);
                    return Ok(p);
                }
            }
            bail!(
                "no available ports in range {}..={}",
                self.range.0,
                self.range.1
            );
        } else {
            if port < self.range.0 || port > self.range.1 {
                bail!(
                    "port {port} out of allowed range {}..={}",
                    self.range.0,
                    self.range.1
                );
            }
            if used.contains(&port) {
                bail!("port {port} already in use");
            }
            used.insert(port);
            Ok(port)
        }
    }

    /// 释放端口
    #[allow(dead_code)]
    pub fn release(&self, port: u16) {
        self.used.lock().unwrap().remove(&port);
    }
}
