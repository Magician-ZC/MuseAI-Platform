//! 任务队列抽象：tick 调度与通知 outbox 消费共用。
//! dev/test 用内存实现；Redis 实现留为后续接入位（trait 不变）。

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::BinaryHeap;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

#[async_trait]
pub trait Queue: Send + Sync {
    /// 入队（due_ms 为期望执行时间；<=now 立即可取）
    async fn push(&self, topic: &str, payload: String, due_ms: i64);
    /// 阻塞取一条到期任务（按 due_ms 顺序）；无任务时挂起等待
    async fn pop(&self, topic: &str) -> String;
}

pub async fn push_json<T: Serialize>(q: &dyn Queue, topic: &str, value: &T, due_ms: i64) {
    if let Ok(s) = serde_json::to_string(value) {
        q.push(topic, s, due_ms).await;
    }
}

pub async fn pop_json<T: DeserializeOwned>(q: &dyn Queue, topic: &str) -> Option<T> {
    let raw = q.pop(topic).await;
    serde_json::from_str(&raw).ok()
}

#[derive(Eq, PartialEq)]
struct Item {
    due_ms: i64,
    seq: u64,
    payload: String,
}
impl Ord for Item {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap 是大顶堆，反转得到最早到期优先
        other.due_ms.cmp(&self.due_ms).then(other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for Item {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
struct TopicState {
    heap: BinaryHeap<Item>,
    seq: u64,
}

/// 内存队列：进程内多 topic 延时队列。
#[derive(Default)]
pub struct MemQueue {
    topics: Mutex<std::collections::HashMap<String, TopicState>>,
    notify: Notify,
}

impl MemQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl Queue for MemQueue {
    async fn push(&self, topic: &str, payload: String, due_ms: i64) {
        {
            let mut topics = self.topics.lock().await;
            let state = topics.entry(topic.to_string()).or_default();
            state.seq += 1;
            let seq = state.seq;
            state.heap.push(Item { due_ms, seq, payload });
        }
        self.notify.notify_waiters();
    }

    async fn pop(&self, topic: &str) -> String {
        loop {
            let now = crate::db::now_ms();
            let wait_ms = {
                let mut topics = self.topics.lock().await;
                let state = topics.entry(topic.to_string()).or_default();
                match state.heap.peek() {
                    Some(item) if item.due_ms <= now => {
                        return state.heap.pop().expect("peeked").payload;
                    }
                    Some(item) => (item.due_ms - now).clamp(10, 5_000) as u64,
                    None => 5_000,
                }
            };
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(wait_ms),
                self.notify.notified(),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pops_in_due_order() {
        let q = MemQueue::new();
        q.push("t", "b".into(), crate::db::now_ms() + 20).await;
        q.push("t", "a".into(), crate::db::now_ms() - 1).await;
        assert_eq!(q.pop("t").await, "a");
        assert_eq!(q.pop("t").await, "b");
    }
}
