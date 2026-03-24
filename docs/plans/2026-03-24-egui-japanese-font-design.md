# egui日本語フォント対応 Design

**Date:** 2026-03-24
**Discovery Log:** `docs/plans/2026-03-24-egui-japanese-font-discovery.md`
**Status:** approved

---

## Overview

egui版のMarkdownビューアーで日本語テキストが豆腐（□）になる問題を修正する。Noto Sans JP Regular を `include_bytes!` でバイナリに埋め込み、eguiのフォントフォールバックとして登録する。

## Authoritative Inputs

- Discovery log: `docs/plans/2026-03-24-egui-japanese-font-discovery.md`
- 個人用途のfork、upstream PRは目指さない
- サイズ制約なし、シンプルさ優先

## Goals

- egui版の全テキスト（Markdownコンテンツ、TOC、検索バー等）で日本語が正しく表示される
- Noto Sans JP Regular を `include_bytes!` でバイナリに埋め込む
- 単一自己完結バイナリを維持

## Non-Goals

- CJK全般のサポート（日本語のみ）
- webview/tuiバックエンドへの変更
- upstream PR対応
- フォントが利用できない場合のエラーハンドリング
- SVG/Mermaid描画のフォント対応（usvg::fontdbは既にsystem fontsをロード済み）

## Chosen Approach

Noto Sans JP Regular の `.ttf` ファイルをリポジトリに配置し、`include_bytes!` でコンパイル時にバイナリへ埋め込む。`eframe::App::setup()` 内で `egui::FontDefinitions` にフォントを追加し、`Proportional` と `Monospace` のフォールバックとして登録。

## Alternatives Considered

### Option B: システムフォントを動的にロード
- Pros: バイナリサイズ増加なし
- Cons: OS依存、フォントがない環境で豆腐のまま、「バンドル」要件に合わない

### Option C: Noto Sans JP のサブセットを埋め込み
- Pros: サイズ最小化（1-3MB）
- Cons: サブセット作成に追加ツール必要、「サイズは気にしない」のため不要な複雑さ

### Recommendation
Option A（フルフォント埋め込み）: サイズ気にしない＋シンプルさ優先＋単一バイナリの要件にぴったり合致。

## Architecture

### ファイル配置
```
assets/fonts/
├── NotoSansJP-Regular.ttf
└── OFL.txt
```

### コード変更: `src/backend/egui.rs` のみ

1. `include_bytes!` でフォントデータを定数として埋め込み
2. `MarkdownApp::setup()` 内で `egui::FontDefinitions` を構成
3. `Proportional` / `Monospace` ファミリーのフォールバック末尾に追加
4. `ctx.set_fonts()` で適用

## Data / State Flow

1. コンパイル時: `NotoSansJP-Regular.ttf` → `include_bytes!` → バイナリに埋め込み
2. 起動時: `setup()` → `FontDefinitions` 構成 → `ctx.set_fonts()` → eguiフォントシステムに登録
3. 描画時: ラテン文字 → デフォルトフォント、日本語 → Noto Sans JP にフォールバック

## Error Handling

- ベストエフォート。フォントは静的に埋め込まれるため、ランタイムエラーは発生しない。

## Testing Strategy

- 手動テスト: 日本語Markdownファイルでegui版を起動し確認
  - 本文（ひらがな、カタカナ、漢字）
  - 見出し（TOCに反映されるか）
  - コードブロック内の日本語コメント
  - 検索バーへの日本語入力
- ビルド確認: `cargo build --features egui-backend` が成功すること

## Perspective-Specific Notes

### Product / Value
個人用途の日本語Markdown閲覧がスムーズになる。

### Security / Risk
フォントはOFL（SIL Open Font License）で配布。セキュリティリスクなし。

### Maintainability / Operations
fork固有。フォントファイル更新時はttfを差し替えるだけ。

### UX / Workflow
全egui UIテキストで日本語が表示可能に。追加設定不要。

### Architecture / Integration
egui.rsのみの変更。他バックエンドに影響なし。egui_commonmarkはeguiのフォントシステムを使うため追加設定不要。

## Open Questions / Explicit Tradeoffs

- なし（すべてdiscoveryで解決済み）
