# SerialMonitorEssential MCP サーバー

AI エージェント（Claude Code など）が **SerialMonitorEssential の GUI アプリが開いているシリアルセッションを、そのまま読み書きする**ための MCP (Model Context Protocol) stdio サーバーです。

## 目的

COM ポートは排他デバイスで、同時に開けるプロセスはひとつだけです。
そこでこの構成では **アプリ本体がマルチプレクサ**になります。

```
  デバイス ── COM ポート ── SerialMonitorEssential (アプリが唯一の所有者)
                                  ├── GUI ............ 人間が見る・送る
                                  └── AI Bridge (TCP) ── MCP サーバー ── AI エージェント
```

人間は GUI、AI は MCP。**同じ受信バッファ・同じポート**を見ているので、
「人が眺めているログを AI にそのまま調べさせる」「AI にコマンドを打たせて人が横で確認する」
といった作業が、ポートの奪い合いなしに成立します。

| レイヤ | 実体 | 役割 |
| --- | --- | --- |
| Layer 1 | アプリ内の AI Bridge | `127.0.0.1:57320` に NDJSON の TCP サーバーを立てる |
| Layer 2 | 本ディレクトリ (`server.mjs`) | その TCP を MCP ツールに変換する stdio サーバー |

## 前提

1. **SerialMonitorEssential を起動しておく**こと。
2. **設定画面で「AI Bridge」を ON** にすること（既定は OFF）。
3. Node.js **20 以上**。
4. 依存関係のインストール:

   ```bash
   cd mcp
   npm install
   ```

アプリが起動していない／AI Bridge が OFF のときは、ツールは例外ではなく
「アプリを起動して AI Bridge を ON にしてください」という日本語＋英語のエラーメッセージを返します。

## Claude Code への登録

```bash
claude mcp add serial-monitor -- node "C:/Users/kazuki/Documents/SerialMonitorEssential/mcp/server.mjs"
```

環境変数を渡す場合は `-e` を使います。

```bash
claude mcp add serial-monitor -e SME_BRIDGE_PORT=57320 -- node "C:/Users/kazuki/Documents/SerialMonitorEssential/mcp/server.mjs"
```

`.mcp.json` に直接書く場合:

```json
{
  "mcpServers": {
    "serial-monitor": {
      "command": "node",
      "args": ["C:/Users/kazuki/Documents/SerialMonitorEssential/mcp/server.mjs"],
      "env": {}
    }
  }
}
```

登録後、`claude mcp list` に `serial-monitor` が出れば OK です。

## 環境変数

| 変数 | 既定値 | 説明 |
| --- | --- | --- |
| `SME_BRIDGE_HOST` | `127.0.0.1` | AI Bridge のホスト。ループバック以外は非推奨。 |
| `SME_BRIDGE_PORT` | `57320` | AI Bridge のポート。アプリ側の設定と合わせる。 |
| `SME_BRIDGE_TOKEN` | (なし) | アプリ側でトークンを設定した場合のみ。接続直後に `auth` を自動送信します。 |

## ツール一覧

| ツール | 引数 | 返すもの |
| --- | --- | --- |
| `serial_status` | なし | 接続状態・ポート名・受信バイト数・アプリバージョン（要約＋生 JSON） |
| `serial_ports` | なし | PC が認識しているシリアルポートの一覧 |
| `serial_read_tail` | `bytes?` (既定 4096 / 最大 1048576) | 直近の受信データ。テキストならそのまま、バイナリらしければ 16 バイト/行の hex ダンプ。offset と total_bytes 付き |
| `serial_read_range` | `offset`, `length` (最大 1048576) | 指定範囲の受信データ。表示ルールは `serial_read_tail` と同じ |
| `serial_send` | `text`, `line_ending?` (`none`/`cr`/`lf`/`crlf`、既定 `lf`) | 書き込んだバイト数。**送信内容は GUI にも表示されます** |
| `serial_send_hex` | `hex` (例 `"01 03 00 00 00 0A"`) | 16進ペアを検証して生バイトを送信。改行は付きません |
| `serial_wait_for` | `pattern`, `timeout_ms?` (既定 10000), `from_end?` (既定 true) | **本命機能。** 呼び出し後に届いた新規データを 500ms 間隔でポーリングし、正規表現に一致したら該当箇所と offset を返す。タイムアウト時は直近 256 バイトを表示 |

補足:

- `serial_wait_for` の `pattern` は JavaScript の正規表現ソース（`m` フラグ付きで評価）。
- `from_end: false` にすると、既にバッファに溜まっている直近 4096 バイトも検索対象に含めます。
- バイナリ判定は「印字不可バイトが 10% 超」。UTF-8 として妥当な日本語ログはテキスト扱いになります。

## 典型的なフロー

```
1. serial_status            → ポートが開いているか、今何バイト受信しているかを確認
2. serial_send              → "AT+VER" などのコマンドを送る（GUI にも表示される）
3. serial_wait_for          → "OK|ERROR" 等の応答を待つ（送信直後の新規データだけを対象）
4. serial_read_tail         → 前後の文脈をもう少し広く読む
```

エージェントへの指示例:

> `serial_status` でポートを確認して、`serial_send` で `AT+VER` を送り、
> `serial_wait_for` で `VER:.*` を 5 秒待って、返ってきたバージョンを教えて。

範囲を絞って読み直したいときは、`serial_wait_for` が返した `offset` を
`serial_read_range` にそのまま渡せます。

## セキュリティ

- **待ち受けは `127.0.0.1` のみ。** 外部ネットワークからは接続できません。
- **既定は OFF。** アプリの設定で明示的に AI Bridge を ON にしたときだけ動きます。
- **送信は必ず GUI に表示されます。** AI が何を書き込んだか、人間が画面で確認できます。
- 必要に応じて `SME_BRIDGE_TOKEN` でトークン認証を有効にできます（同一 PC 上の他プロセス対策）。
- MCP サーバー自身はファイルを読み書きしません。標準出力は MCP プロトコル専用で、ログはすべて標準エラーに出ます。

## 開発・動作確認

```bash
node --check server.mjs   # 構文チェック
node smoke.mjs            # ブリッジ側を偽装した結合テスト（PASS 行が出て exit 0）
npm start                 # MCP サーバーを単体起動（stdin EOF で終了）
```

`smoke.mjs` は MCP を使わず、エフェメラルポートに**ブリッジ側の偽サーバー**を立てて
`server.mjs` のブリッジクライアント（`BridgeClient` / `waitForPattern` / レンダリング関数）を検証します。
status のラウンドトリップ、base64 デコード、改行付き送信、`wait_for` のポーリング一致、
アプリ未起動時の即時フェイルとメッセージ内容までカバーしています。
