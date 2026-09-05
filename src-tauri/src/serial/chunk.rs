/// Chunk: データ受信の基本単位
///
/// 高速データ受信のために固定サイズのバッファを持ち、
/// データが満杯になるか16ms経過したらスワップする。
/// タイムスタンプ管理はByteTimestamp（data_store.rs）で行う。
pub struct Chunk {
    buffer: Box<[u8]>,
    capacity: usize,
    valid_len: usize,
    global_offset: u64,
}

impl Chunk {
    /// 新しいChunkを作成
    ///
    /// # Arguments
    /// * `capacity` - バッファサイズ（デフォルト: 64KB）
    pub fn new(capacity: usize) -> Self {
        let buffer = vec![0u8; capacity].into_boxed_slice();
        Self {
            buffer,
            capacity,
            valid_len: 0,
            global_offset: 0,
        }
    }

    /// データを追加
    ///
    /// # Arguments
    /// * `data` - 追加するデータ
    ///
    /// # Returns
    /// 実際に追加されたバイト数
    pub fn push_data(&mut self, data: &[u8]) -> usize {
        let available = self.capacity - self.valid_len;
        let to_copy = data.len().min(available);

        if to_copy > 0 {
            self.buffer[self.valid_len..self.valid_len + to_copy].copy_from_slice(&data[..to_copy]);
            self.valid_len += to_copy;
        }

        to_copy
    }

    /// Chunkが満杯かどうか
    pub fn is_full(&self) -> bool {
        self.valid_len >= self.capacity
    }

    /// 有効なデータが存在するか
    pub fn has_data(&self) -> bool {
        self.valid_len > 0
    }

    /// 有効データの参照を取得
    pub fn data(&self) -> &[u8] {
        &self.buffer[..self.valid_len]
    }

    /// データ長を取得
    pub fn len(&self) -> usize {
        self.valid_len
    }

    /// グローバルオフセットを設定
    pub fn set_global_offset(&mut self, offset: u64) {
        self.global_offset = offset;
    }

    /// グローバルオフセットを取得
    pub fn global_offset(&self) -> u64 {
        self.global_offset
    }

    /// Chunkをクリアして再利用可能にする（テスト用）
    /// Note: Arc設計でChunkは自動drop。プール返却不要のため実運用では未使用
    #[cfg(test)]
    pub fn clear(&mut self) {
        self.valid_len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_new() {
        let chunk = Chunk::new(1024);
        assert_eq!(chunk.capacity, 1024);
        assert_eq!(chunk.valid_len, 0);
        assert!(!chunk.has_data());
        assert!(!chunk.is_full());
    }

    #[test]
    fn test_chunk_push_data() {
        let mut chunk = Chunk::new(10);
        let data = b"Hello";
        let written = chunk.push_data(data);

        assert_eq!(written, 5);
        assert_eq!(chunk.len(), 5);
        assert_eq!(chunk.data(), b"Hello");
        assert!(chunk.has_data());
        assert!(!chunk.is_full());
    }

    #[test]
    fn test_chunk_full() {
        let mut chunk = Chunk::new(5);
        chunk.push_data(b"12345");

        assert!(chunk.is_full());
        assert_eq!(chunk.len(), 5);

        // オーバーフロー試行
        let overflow = chunk.push_data(b"678");
        assert_eq!(overflow, 0); // 追加されない
        assert_eq!(chunk.len(), 5);
    }

    #[test]
    fn test_chunk_partial_write() {
        let mut chunk = Chunk::new(10);
        chunk.push_data(b"12345");

        // 残り5バイトだが7バイト書き込もうとする
        let written = chunk.push_data(b"abcdefg");
        assert_eq!(written, 5); // 5バイトだけ書き込まれる
        assert_eq!(chunk.len(), 10);
        assert!(chunk.is_full());
        assert_eq!(chunk.data(), b"12345abcde");
    }

    #[test]
    fn test_chunk_clear() {
        let mut chunk = Chunk::new(10);
        chunk.push_data(b"Hello");

        assert_eq!(chunk.len(), 5);

        chunk.clear();
        assert_eq!(chunk.len(), 0);
        assert!(!chunk.has_data());
        assert!(!chunk.is_full());
    }

    #[test]
    fn test_chunk_empty_data() {
        let mut chunk = Chunk::new(10);
        let written = chunk.push_data(b"");

        assert_eq!(written, 0);
        assert_eq!(chunk.len(), 0);
        assert!(!chunk.has_data());
    }
}
