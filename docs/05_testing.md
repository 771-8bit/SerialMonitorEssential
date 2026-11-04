# テスト方針 (Testing Policy)

## Rustバックエンド

### 単体テスト
```bash
cd src-tauri
cargo test --lib
```

### Linting & Formatting
```bash
cd src-tauri
cargo clippy --lib
cargo fmt -- --check
```

## Frontend (TypeScript)

### Type Checking & Linting
```bash
npm run type-check
npm run lint
```

## 継続的インテグレーション (CI)

GitHub Actionsにより、Pull Request作成時とpush時に以下が自動実行されます：
- `cargo test --lib`
- `cargo clippy --lib`
- `cargo fmt -- --check`
- `npm run tauri build`

## E2E / 実機テスト

Pythonスクリプトを用いた実機テスト（Raspberry Pi Pico）や仮想COMポートテストの手順は、[test_tools/README.md](../test_tools/README.md) を参照してください。

---

## 関連ドキュメント

- [プロジェクト概要](01_overview.md)
- [実装ロードマップ](06_roadmap.md)
