# `athena init` → `athena-api` 起動失敗の原因調査報告書

| 項目 | 内容 |
|---|---|
| 指示ソース | `.aiprj/instructions.md`（最優先事項、2026-11-07 時点の内容） |
| 指示内容 | `athena init` の後に `athena-api` を起動したところエラーが発生したため、原因を調査して報告する |
| 調査対象 | `athena init` が生成する `.athena/config.yaml`、`athena-config::validate`、関連仕様規定 |
| 調査日 | 2026-11-07 |
| 調査対象バージョン | workspace `version = "0.8.0"`（commit `2375801`、tag `v0.8.0`） |

---

## 1. 結論（要旨）

**報告されたエラー自体は「設計どおりの意図的な失敗」であり、不具合ではありません。** ATHENA は
プロバイダ／モデルの既定値を持たない方針（`ATH-AGENT-008`）のため、`athena init` 直後の設定
ファイルは意図的に起動不能な状態で生成されます。したがって起動には運用者による値の記入が
必要です。

**ただし、その「記入すべき値」の案内が誤っており、これが実質的な不具合です。**
生成される `config.yaml` の冒頭コメントは「**2 つ**の値（`core.agent.provider` と
`core.agent.model`）を記入すれば検証を通る」と明言していますが、実際には **3 つ目**
（`core.agent.delegable_tools`）を記入しなければ次のエラーで停止します。利用者は指示に従って
2 つを記入した直後に、案内されていない第 2 のエラーに遭遇します。

---

## 2. 再現手順と観測事実

空のディレクトリで実行しました。

### 手順 1：`athena init`

```console
$ athena init
athena init: created 44 element(s), kept 0 existing element(s)
```

`.athena/` が生成され、`config.yaml` の `core.agent` は次の状態でした（抜粋）。

```yaml
core:
  agent:
    persona: "${athena.home}/personas/producer.md"
    workspace: "${athena.home}/var/core-agent"
    runtime: "agent-cli"
    provider: "" # MUST set — no ATHENA-side default (ATH-AGENT-008)
    model: ""    # MUST set — no ATHENA-side default (ATH-AGENT-008)
    skills: []
    restart:
      max_attempts: 3
      on_exhausted: "escalate"
    # Minimal by default; shell / process / network never belong here (ATH-SEC-025).
    allowed_tools: ["fs_read", "fs_write", "send_to"]
    # Empty by default: the permission ceiling must be stated explicitly (ATH-SEC-024).
    delegable_tools: []
```

### 手順 2：`athena-api`（報告されたエラー）

```console
$ athena-api
athena-api: configuration failed: configuration constraint violated: core.agent.provider must be set explicitly; ATHENA holds no provider default so that cost and quality stay judgeable from configuration alone
```

これは報告いただいた文言と一致します。

### 手順 3：案内どおりに「2 つ」だけ記入して再実行

`core.agent.provider` と `core.agent.model` のみを記入しました。

```yaml
    provider: "ollama"
    model: "glm-5.3-flash:cloud"
```

```console
$ athena-api
athena-api: configuration failed: configuration constraint violated: core.agent.delegable_tools must be stated explicitly and non-empty; it is ATHENA's overall permission ceiling, and an empty set leaves every subordinate unable to act
```

**案内されていない第 2 のエラーで停止しました。**

### 手順 4：3 つ目も記入

```yaml
    delegable_tools: ["fs_read", "fs_write", "shell", "git", "send_to"]
```

```console
$ athena-api
athena-api: ready on 127.0.0.1:8080 (store: /tmp/probe2/.athena/var/athena.db)
athena-api: Core Agent resident (instance #0, runtime ri-01M6V6D0B9M75TV7QWCGRW5GWN)
```

起動に成功しました。以上より、**起動に必要な記入値は 3 つ**であることが実測で確定しました。

---

## 3. 原因分析

### 3.1 直接原因 — 意図的な fail-fast（不具合ではない）

失敗の直接原因は `crates/athena-config/src/validate.rs:103-108` の検査です。

```rust
// ATH-AGENT-008: no ATHENA-side default exists, so an unset value is a validation error
// rather than something to fill in.
if config.core.agent.provider.trim().is_empty() {
    return Err(fail(
        "core.agent.provider must be set explicitly; ATHENA holds no provider default so \
         that cost and quality stay judgeable from configuration alone"
            .into(),
    ));
}
```

仕様 `ATH-AGENT-008` は「ATHENA はプロバイダ既定値を持たない」と定めており、空値は
「選べという催促」として意図的に失敗させています。**この挙動は仕様どおりです。**

### 3.2 実質的な不具合 — 「2 つ」という案内が誤り

`athena init` が生成する設定ファイルの冒頭には、次の記述があります
（`crates/athena-cli/src/default-config.yaml` の 6〜9 行目）。

```
# ATHENA holds no provider or model default (ATH-AGENT-008): fill in
# `core.agent.provider` and `core.agent.model` before the first start. A fresh copy of
# this file therefore validates apart from those two values, by design — the empty value
# is the fail-fast reminder to choose.
```

「**apart from those two values**（この 2 つの値を除いて検証を通る）」と明言しています。しかし
実測したとおり、2 つを記入しても

```rust
if config.core.agent.delegable_tools.is_empty() {
    return Err(fail(
        "core.agent.delegable_tools must be stated explicitly and non-empty; ..."
    ));
}
```

（`crates/athena-config/src/validate.rs:129-135`）で停止します。**生成された案内文が、実際の
検証器の要求と一致していません。**

### 3.3 矛盾の所在（3 箇所）

同じ「必要値」について、リポジトリ内で 3 つの記述が食い違っています。

| # | 箇所 | 記述 | 内容 |
|---|---|---|---|
| 1 | `crates/athena-cli/src/default-config.yaml`（冒頭コメント） | 「2 つの値を除いて検証を通る」 | **誤り**（実際は 3 つ） |
| 2 | `tests/init_defaults.rs` の doc コメント | 「the two values the operator must supply」 | **誤り**（同テストの本体は 3 つ設定している） |
| 3 | `README.md` / `README_ja.md` | 「運用者が供給する **3 つの値**」 | **正しい** |

とくに #2 は自己矛盾です。`tests/init_defaults.rs` の
`generated_default_config_passes_validation_once_operator_values_are_set` は、実際には

```rust
parsed.core.agent.provider = "claude".into();
parsed.core.agent.model = "claude-opus-4-7".into();
parsed.core.agent.delegable_tools = vec![ToolName::FsRead, ToolName::FsWrite, ToolName::Shell];
athena_config::validate(&parsed).expect("...");
```

と **3 つ**を設定してから検証しています。つまりテストの本体は正しく 3 つであることを示して
いるのに、その doc コメントと `default-config.yaml` の案内文だけが「2 つ」と書いています。

### 3.4 なぜテストで検出されなかったか

`athena-cli` は依存グラフの制約（設計 §3.2）により `athena-config` に依存できないため、生成物
と検証器の突き合わせは `tests/init_defaults.rs` が担っています。しかしこのテストは

- 生成物がスキーマとして解釈できること（第 1 テスト）
- 3 値を設定すれば検証を通ること（第 2 テスト）

は確認しますが、**「生成物に書かれた案内文（コメント）と実際に必要な値の個数が一致するか」は
確認していません**。コメントは YAML の意味論から外れるため、スキーマ検査でも検出できません。
これが、誤った案内が残り続けた理由です。

---

## 4. 仕様との関係（グレーゾーンの指摘）

`delegable_tools` の扱いには、仕様の文言と実装の間に解釈の差があります。

| 出典 | 記述 |
|---|---|
| `ATH-SEC-024`（仕様） | `delegable_tools` は設定で明示しなければならない。**既定は空集合とする** |
| 実装（`validate.rs`） | 空集合は**検証エラー**（明示されていても空なら拒否） |

仕様は「既定は空集合」と定めているため、空集合そのものは仕様に反しません。一方、実装は
「空集合では下位 Agent が一切動けず階層が仕事にならない」という `ATH-SEC-009` の趣旨から、
空集合を拒否しています。**この厳格化は理にかなっていますが、仕様の文言より厳しい**ため、
「既定のまま起動できない」という今回の挙動を生んでいます。仕様側に「空集合のままでは起動
できない（運用者が上限を確定するまで起動不可）」と明記することが望ましいと考えます。

---

## 5. 影響範囲

| 対象 | 影響 |
|---|---|
| 新規に `athena init` する利用者 | **全員が遭遇します。** 案内どおり 2 つ記入すると第 2 のエラーで停止し、原因究明に時間を要します |
| 既存プロジェクト | 影響なし（既に 3 値が記入済み） |
| `athena update` | 影響なし（`config.yaml` を変更しない） |
| セキュリティ | 影響なし。むしろ権限上限の明示を強制する側に倒れており、安全側です |

重大度は **中** と評価します。起動は必ず失敗し、回避方法（3 つ目の記入）も README には
記載があるため作業不能にはなりませんが、**製品自身が出力する案内が誤っている**ため、
利用者を確実に迷わせます。初回導入時の離脱要因になり得ます。

---

## 6. 推奨される対処

優先度順に記載します。

1. **（必須）`default-config.yaml` の冒頭コメントを「3 つ」に修正する。**
   「apart from those two values」を「apart from those three values」とし、
   `core.agent.delegable_tools` を明示的に列挙する。あわせて `tests/init_defaults.rs` の
   doc コメントも同様に修正する。
2. **（推奨）案内文と実装の一致を機械的に検査する回帰テストを追加する。**
   生成物のコメントから「必要値の個数」を読み取って検証器の要求と突き合わせる、あるいは
   より単純に「生成物の空値キー集合」と「`validate` が拒否するキー集合」を比較する形が
   考えられます。
3. **（推奨）`ATH-SEC-024` の仕様記述を明確化する。**
   「既定は空集合だが、空集合のままでは起動できない」旨を明記し、実装の厳格化を仕様に
   裏づける（`ATH-GEN-006` の仕様改訂手続きによる）。
4. **（任意）`athena init` の完了メッセージに、必要な 3 値を作業項目として列挙する。**
   現在は `created 44 element(s)` とのみ表示され、次に何をすべきかが出力されません。

---

## 7. 調査方法

- 空ディレクトリにて `athena init` → `athena-api` を実行し、エラーを再現（手順 1〜2）。
- 案内文どおり 2 値のみ記入して再実行し、第 2 のエラーを実測（手順 3）。
- 3 値目を記入して起動成功を確認し、必要値が 3 つであることを確定（手順 4）。
- `crates/athena-config/src/validate.rs` の検査順序を読み、どの制約がどの順で発火するかを確認。
- `crates/athena-cli/src/default-config.yaml` の案内文、`tests/init_defaults.rs`、README 対の
  記述を突き合わせ、3 箇所の不一致を特定。
- `git log` により、当該検査と既定設定が初回 import（`08ff5c9`, 2026-10-18）以来 unchanged
  であること、すなわち本件が後退（リグレッション）ではなく**当初から存在した記載不備**で
  あることを確認。
- 仕様 `spec/athena_spec_1.0.md` の `ATH-AGENT-008` / `ATH-SEC-009` / `ATH-SEC-024` /
  `ATH-SEC-025` の条文と照合。

本調査中、製品コード・設定・仕様のいずれも変更していません（読み取りと、一時ディレクトリでの
再現実行のみ）。
