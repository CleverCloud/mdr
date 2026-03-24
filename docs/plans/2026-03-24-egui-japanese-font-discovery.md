# egui日本語フォント対応 Discovery Log

**Date:** 2026-03-24
**Topic Slug:** egui-japanese-font
**Perspective Config:** bundled default (multi-perspective.default.json)
**External Engine:** codex
**Status:** ready-for-design

---

## Raw User Input (Verbatim)

> egui versionについて日本語のフォントが入っておらず、豆腐になってしまいます。それを是正したいです。brainstormingしてください。また、forkして自分のrepoでやりたいです

---

## Project Context Summary

- **mdr**: Rust製の軽量Markdownビューアー（v0.2.8）
- 3つのバックエンド: egui（ネイティブGUI）、webview（HTML/WebKit）、tui（ターミナル）
- egui版は`eframe 0.33` + `egui_commonmark 0.22`を使用
- **現状**: egui版にカスタムフォント設定が一切なく、`egui::FontDefinitions`の設定もない
- **webview版**: ブラウザエンジン経由でOSのシステムフォントを利用するため、日本語表示は通常可能
- **フォントファイル**: プロジェクトにフォントファイルは同梱されていない
- **SVG描画**: `usvg::fontdb`で`load_system_fonts()`のみ使用

---

## Authoritative Decisions

- ユーザーはforkして自分のrepoで作業したい
- 対象はegui版の日本語フォント豆腐問題
- 言語カバレッジ: 日本語のみ
- フォント取得方法: アプリ/リポジトリにバンドル
- フォント未利用時のUX: ベストエフォートで構わない
- upstream互換性: fork固有で構わない、個人用途で手元で使うだけ
- バイナリサイズ: 気にしない、最もシンプルで確実な方法を優先
- パッケージング: 単一自己完結バイナリ（include_bytes!埋め込み）
- フォント方向性: 標準的なサンセリフ日本語UIフォント（Noto Sans JPなど）
- 日本語表示範囲: egui app内のすべてのテキスト（UI要素含む）

---

## Question Rounds

### Round 1

**Perspectives used:** product, architecture, ux, maintainability

**Generated Questions**
1. [Product] egui版で対象とする言語カバレッジはどの範囲ですか？
2. [Architecture] フォントの取得方法はどうしますか？
3. [UX] 日本語対応フォントがランタイムで利用できない場合、許容されるUXは？
4. [Maintainability] forkのソリューションをupstreamに戻しやすくすることの重要度は？

**User Answers (Verbatim)**
1. a（日本語のみ）
2. a（アプリ/リポジトリにフォントをバンドルする）
3. c（ベストエフォートで一部環境で失敗しても構わない）
4. b（問題がきれいに解決できるならfork固有で構わない）個人用途で、手元で使うだけです

**Derived Durable Notes**
- 日本語のみをスコープとする（CJK全般ではない）
- フォントはバイナリに同梱する方式
- フォント未利用時のフォールバックは不要（ベストエフォート）
- upstream PRは目指さない、fork固有の変更OK
- 個人用途のためシンプルな実装が優先

### Round 2

**Perspectives used:** maintainability, architecture, product, ux

**Generated Questions**
1. [Maintainability] 日本語フォントバンドルによるバイナリサイズ増加の許容範囲は？
2. [Architecture] バンドルフォントのパッケージング方式は？
3. [Product] 好みの日本語フォントの方向性は？
4. [UX] egui版のどこで日本語表示が必要ですか？

**User Answers (Verbatim)**
1. c（サイズは気にしない。最もシンプルで確実な方法を優先）
2. a（単一の自己完結バイナリ）
3. a（標準的なサンセリフ日本語UIフォント、Noto Sans JPなど）
4. b（egui app内のすべてのテキスト）

**Derived Durable Notes**
- バイナリサイズ増加は許容、シンプルさ優先
- include_bytes!でバイナリに直接埋め込む
- Noto Sans JP（サンセリフ）が適切
- Markdownコンテンツだけでなく、TOC・検索バーなどUI要素全体で日本語対応が必要

---

## Open Questions

_(なし — デザインに進める状態)_

---

## Ready for Design When

- [x] Goals and non-goals are clear
- [x] Major constraints are explicit
- [x] Blocking ambiguities are resolved or acknowledged
- [x] The next step is design, not more discovery
