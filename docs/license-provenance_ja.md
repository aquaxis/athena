# ライセンス来歴(P-lic 記録。`ATH-DEV-019`、X-07)

| 項目 | 内容 |
|---|---|
| ライセンス | MIT — リポジトリルートの `LICENSE`(`aiprj` および `agent-cli` と同一) |
| 対象 | リポジトリ内のすべての第三者材料。取り込み前と取り込み時の両方 |
| 英語版 | `docs/license-provenance.md`(同一内容) |

## 1. ベンダー_import した第三者材料

| 材料 | 出所 | バージョン | ライセンス | 取り込み時の状態 |
|---|---|---|---|---|
| `aiprj` 展開内容(instructions.md の種、`rules/`、`.agent-cli/` のプロジェクトオーバーレイ、commands) | `aquaxis/aiprj`(https://github.com/aquaxis/aiprj) | 1.1.0 | MIT | ファイルは意図的な読み取り指示の書き換え(`ATH-AIPRJ-028` の絶対パス形式)を除き未変更。来歴は `crates/athena-cli/src/template.rs` に記録。`.claude/` と上流の README は ATHENA の展開に含めない(`ATH-CLI-009`) |
| `aiprj` のコマンド手順ファイル(`ai.md` / `next_ai.md` / `close_ai.md`) | `aquaxis/aiprj` 1.1.0 | MIT | 未変更。Runtime は slash command ではなく(`ATH-ACLI-016`)、絶対パスで読む手順ファイルとして扱う(`ATH-ACLI-017`) |

## 2. Cargo 依存

workspace の依存は crates.io 由来で、上流のライセンスを維持する(主要インフラクレート —
tokio、axum、serde、rusqlite、schemars、ulid、sha2、thiserror — の MIT / Apache-2.0 の
デュアルセットと、互換ライセンスのリーフクレート)。依存をツリー内で fork しない。すべて
`Cargo.toml` に宣言され、CI の依存境界ジョブがこれを解析する(`ATH-DEV-011`)。取り込み
候補のライセンス変更は、依存追加の前に MIT との適合性を確認する。

## 3. Skill

Phase 8 の開発状態までに、第三者の Skill Package ソースは `<ATHENA_HOME>/skills/` へ
取り込まれていない。将来の取り込みは `ATH-SKILL-035`/`036` に従う: 最初にライセンス適合性の
review、登録時に publisher/source の allow-list、manifest に対するチェックサム検証、
取り込み時点で本記録へ来歴を追記する。
