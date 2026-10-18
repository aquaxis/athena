# Skill の Agent 到達 — 確定したメカニズム(open item O-2)

| 項目 | 内容 |
|---|---|
| 状態 | 確定(P5-08) |
| 根拠要件 | `ATH-SKILL-017`(Loader の配置)、`ATH-ACLI-015`(禁止パス)、`ATH-SKILL-006`(registered のみ候補) |
| 修正 | 仕様 §12.2.1 新設 / `ATH-SKILL-039` 追加(本書 §2) |
| 英語版 | `docs/skill_delivery.md`(同一内容) |

## 1. 問題

`agent-cli` には **skill の配置メカニズムが存在しない**(仕様 §2.2。0.23.0 で検証、
`ATH-DEV-022`)。Loader は選択済み Skill を
`<ATHENA_HOME>/var/instances/<instance_id>/skills/<id>/SKILL.md` へ materialize するが
(`ATH-SKILL-017`)、配置された本文は **Agent が読んで**初めて働く。その材料が存在すること、
そしてどこを読むべきかを Agent に伝える者が必要である。

## 2. 確定したメカニズム

到達は、Agent が読むことを検証済みの 2 つの経路に乗る:

1. **Instance の生成済み `.agent-cli/commands/`**。0.23.0 検証(仕様 §2.2.3)により、
   slash-command の本文は `run`(REPL)でのみ展開され — IPC 経路では **Agent がコマンド
   ファイルを絶対パスの手順ファイルとして読む**(`ATH-ACLI-017`)。到達は正確にこの事実を
   利用する: 配置された Skill 毎に、Skill 生成は
   `<agent_cli_dir>/commands/athena-skill-<id>.md` を書き、その内容は 1 つの指示 —
   「`<workspace>/skills/<id>/SKILL.md`(絶対パス、version pinned)を読み、必要時にその
   手順を適用せよ」— である。
2. **生成済み `persona.md`** — Instance の運用規範の節が、そのレベルの常時職務として同じ
   参照を運んでもよい(任意。同一の wiring が生成する)。

材料そのものを文章へ注入することは決してない: メッセージ経路が Agent への指示を運び
(リスクを伴う Skill の使用は承認を経る — `ATH-SKILL-024`、P5-12)、Catalog は Skill を
**名指す**だけである(`ATH-SKILL-002` — Core Agent はパッケージ本文を解釈しない)。

## 3. 規範的な修正(`ATH-SKILL-039`)

**ATH-SKILL-039**(MUST)Skill Manager は、Loader が配置した各 Skill に対し、Instance の
`.agent-cli/commands/athena-skill-<id>.md` を生成し、内容として **絶対パスで** —
`<workspace>/skills/<id>/SKILL.md` を読むよう指示しなければならない。手順ファイルは
`agent-cli` の IPC 経路の読み取り対象(`ATH-ACLI-017`)と同一の機構を用いる。生成先として
`.agent-cli/skills/` を使用してはならない(`ATH-ACLI-015`)。

仕様の改訂履歴に追記された項を参照のこと。
