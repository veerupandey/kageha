//! Turn-local image bytes for a bridge whose packets have a bounded size.
//! Anonymous storage disappears when the client exits. Application checkpoints
//! receive the original messages, so a resumed client can rebuild this index.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
};

#[derive(Default)]
pub(super) struct InlineImages {
    file: Option<File>,
    ranges: HashMap<String, (u64, u64)>,
}

impl InlineImages {
    pub(super) fn pack(&mut self, messages: &mut [Value]) -> io::Result<()> {
        for url in kageha_runtime::turn::image_urls_mut(messages) {
            let Some(data) = url.as_str().filter(|s| s.starts_with("data:image/")) else {
                continue;
            };
            let key = format!("kageha-inline:{:x}", Sha256::digest(data.as_bytes()));
            if !self.ranges.contains_key(&key) {
                if self.file.is_none() {
                    self.file = Some(tempfile::tempfile()?);
                }
                let file = self.file.as_mut().unwrap();
                let offset = file.seek(SeekFrom::End(0))?;
                file.write_all(data.as_bytes())?;
                self.ranges.insert(key.clone(), (offset, data.len() as u64));
            }
            *url = Value::String(key);
        }
        Ok(())
    }

    pub(super) fn restore(&mut self, messages: &mut [Value]) -> io::Result<()> {
        for url in kageha_runtime::turn::image_urls_mut(messages) {
            let Some(&(offset, len)) = url.as_str().and_then(|s| self.ranges.get(s)) else {
                continue;
            };
            let file = self.file.as_mut().unwrap();
            file.seek(SeekFrom::Start(offset))?;
            let mut data = String::with_capacity(len as usize);
            (&mut *file).take(len).read_to_string(&mut data)?;
            if data.len() as u64 != len {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Incomplete turn image",
                ));
            }
            *url = Value::String(data);
        }
        Ok(())
    }
}
