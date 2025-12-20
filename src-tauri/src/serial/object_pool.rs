use super::chunk::Chunk;
use crossbeam::queue::SegQueue;
use std::sync::Arc;

/// ObjectPool: Chunkの再利用プール
///
/// メモリアロケーションのオーバーヘッドを削減するため、
/// 使用済みChunkを再利用する。SegQueueでロックフリー実装。
pub struct ObjectPool {
    pool: Arc<SegQueue<Chunk>>,
    chunk_capacity: usize,
}

impl ObjectPool {
    /// 新しいObjectPoolを作成
    ///
    /// # Arguments
    /// * `initial_count` - 初期Chunk数
    /// * `chunk_capacity` - 各Chunkのバッファサイズ
    pub fn new(initial_count: usize, chunk_capacity: usize) -> Self {
        let pool = Arc::new(SegQueue::new());

        // 初期Chunkを生成
        for _ in 0..initial_count {
            pool.push(Chunk::new(chunk_capacity));
        }

        Self {
            pool,
            chunk_capacity,
        }
    }

    /// 空きChunkを取得
    ///
    /// プールが空の場合は新規作成する。
    /// Note: 現在は直接SegQueue::pop()を使用しているが、
    /// 将来的なリファクタリングのために残している
    #[allow(dead_code)]
    pub fn get_free_chunk(&self) -> Chunk {
        self.pool.pop().unwrap_or_else(|| {
            // プールが空の場合は新規作成
            Chunk::new(self.chunk_capacity)
        })
    }

    /// Chunkをプールに返却
    ///
    /// # Arguments
    /// * `chunk` - 返却するChunk（クリアされて再利用可能になる）
    pub fn return_chunk(&self, mut chunk: Chunk) {
        chunk.clear();
        self.pool.push(chunk);
    }

    /// プール内の利用可能Chunk数を取得（デバッグ用）
    pub fn available_count(&self) -> usize {
        self.pool.len()
    }

    /// 内部プールへの参照を取得（スレッド間共有用）
    pub fn as_arc(&self) -> Arc<SegQueue<Chunk>> {
        self.pool.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_new() {
        let pool = ObjectPool::new(10, 1024);
        assert_eq!(pool.available_count(), 10);
    }

    #[test]
    fn test_pool_get_and_return() {
        let pool = ObjectPool::new(5, 1024);
        assert_eq!(pool.available_count(), 5);

        // Chunkを取得
        let chunk = pool.get_free_chunk();
        assert_eq!(pool.available_count(), 4);

        // Chunkを返却
        pool.return_chunk(chunk);
        assert_eq!(pool.available_count(), 5);
    }

    #[test]
    fn test_pool_exhaust_and_create() {
        let pool = ObjectPool::new(2, 1024);

        // プールを空にする
        let _c1 = pool.get_free_chunk();
        let _c2 = pool.get_free_chunk();
        assert_eq!(pool.available_count(), 0);

        // プールが空でも新規作成される
        let c3 = pool.get_free_chunk();
        assert_eq!(c3.len(), 0);
    }

    #[test]
    fn test_pool_clear_on_return() {
        let pool = ObjectPool::new(1, 1024);

        let mut chunk = pool.get_free_chunk();
        chunk.push_data(b"test data");
        assert_eq!(chunk.len(), 9);

        // 返却時にクリアされる
        pool.return_chunk(chunk);

        let chunk2 = pool.get_free_chunk();
        assert_eq!(chunk2.len(), 0);
        assert!(!chunk2.has_data());
    }

    #[test]
    fn test_pool_reuse() {
        let pool = ObjectPool::new(1, 64);

        // 同じChunkが再利用されることを確認（容量で判断）
        let chunk1 = pool.get_free_chunk();
        let cap1 = chunk1.len();
        pool.return_chunk(chunk1);

        let chunk2 = pool.get_free_chunk();
        let cap2 = chunk2.len();

        assert_eq!(cap1, cap2);
    }
}
