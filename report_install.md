# 新規インストール時（`./install.sh` 実行時）のディレクトリ構成 調査報告

| 項目 | 内容 |
|---|---|
| 指示ソース | `.aiprj/instructions.md`（最優先事項、2026-11-07 時点の内容） |
| 指示内容 | 新規インストール時（`./install.sh` 実行時）のディレクトリ構成を調査し、本書へ日本語で報告する |
| 対応タスク | X-20（R-INST-01、設計 §2.11/D-26） |
| 検証基盤 | 公開リポジトリ `origin/main` = 作業ツリー HEAD `ab3edec`、公開 `install.sh` は作業ツリーと同一ハッシュ（`d799d545…`）。公開されている 1 行インストーラが走らせるのは現在のリポジトリ状態そのもの |
| 実行日時 | 2026-11-07 17:03–17:12 JST |
| 環境 | Linux、rustc 1.95.0、cargo 1.95.0、16 コア、/tmp 31G tmpfs |

---

## 1. 調査方法（経験的実行）

設計 §2.11 の方法面（D-26）に従い、インストールフローを**実際に実行**して各段階で
ディレクトリ構成を観察した。実行した 5 つの顔:

| 顔 | 実行内容 | 新規インストールとの関係 |
|---|---|---|
| **A（公開 1 行インストーラ）** | `curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh \| sh` を空の cwd から実行 | README が示す新規インストールの本来の形 |
| **B（クローン内 `./install.sh`）** | 公開リポジトリを `git clone --depth 1` したツリー内で `./install.sh` を実行 | 指示が名指す `./install.sh` 実行の形 |
| **B2（同・再実行）** | 同じクローンでもう一度 `./install.sh` | 冪等性の確認 |
| **C（プロジェクト初期化）** | 空の新規プロジェクトで `athena init` | インストーラの次ステップそのもの |
| **D（常駐起動）** | 同じプロジェクトで 3 つの運用者設定値を入れ `athena-api` を起動 → 停止 | 次ステップの第 3 面（実行時データの書き込み先） |

前提の確認: 実行前に `~/.cargo/bin` に同じバイナリが存在した（mtime 16:35 の先行プローブ
による）。このため PATH インストールの観察は「**置き換え**」の形となった。置き換え先の
パス・所有・挙動は新規インストール（初回配置）と同一であり、観察結果の構成結論は変わらない。

---

## 2. 段階別の観察結果（フローが生む構成）

### 2.1 段階 0 — 実行前の前提（何もない状態）

- 空の cwd から実行（顔 A）: フローは**実行者の場所に何も作らない**。
- `install.sh` は POSIX `sh` 向け（`set -eu`）。 prerequisite チェック（`cargo`、
  必要時 `rustup`）はディレクトリを一切作らない。

### 2.2 段階 1 — ソース取得（フローの 1 ステップ目）

`install.sh` は 2 つの分岐で「ソースツリーの場所」を決める:

| 分岐 | 条件 | ソースツリーの場所 |
|---|---|---|
| beside-script | 実行した `install.sh` の隣に `Cargo.toml` と `crates/` がある | その隣のリポジトリツリー自身（`./install.sh` のクローン内実行 = 顔 B がこれ） |
| fetch（curl \| sh 側） | 上記がない | **新規に `git clone --depth 1` した一時ディレクトリ配下** |

顔 B のクローンで観察した取得済みツリー（= 取得ステップが生む構成の実体）:

```text
$WORKDIR/athena/                 # clone --depth 1 が作るルート
+-- .git/                        # --depth 1 の浅い履歴
+-- .github/ci.yml               # CI 定義
+-- .gitignore                   # /target など（後述 §2.3）
+-- Cargo.toml / Cargo.lock      # ワークスペース定義
+-- LICENSE                      # MIT
+-- README.md / README_ja.md
+-- install.sh                   # 自分自身のコピー（同一ハッシュ）
+-- rustfmt.toml
+-- agents/producer/{loop.md, core-requests.md}   # Core Agent 手順ファイル
+-- personas/{producer,director,lead,worker}.md   # 配布既定 4 persona
+-- skills/                      # 空ディレクトリ（既定 skill なし）
+-- spec/athena_spec_1.0.md
+-- scripts/build-studio.sh
+-- tests/                       # 16 ファイル（conformance suite）
+-- docs/                        # 20 ファイル（日英ペア+図版）
+-- crates/                      # 15 クレート
+-- runtimes/athena-runtime-agent-cli/
```

**公開セットに含まれないもの（設計 §2.6 の must-not-publish 集合、クローンで全確認）**:
`.aiprj/`・`.agent-cli/`・`.claude/`・`.athena/`・`api.pid`・`target/`・`.agents/`・
`report_review.md` — いずれも取得済みツリーに存在しない。公開リポジトリは ATHENA の
成果物そのものであり、AI 運用領域やローカル実行状態は含まない。

### 2.3 段階 2 — ビルド（release）

- 顔 A（公開側）: ソースを取得した一時ワークツリーの中で `cargo build --release`。
- 顔 B（クローン側）: クローンの中で実行 → **`<クローン>/target/`** にビルド出力が残る
  （release 下に最終バイナリ）。`.gitignore` の `/target` 行がカバーする。
- ビルド時間の実測: 34.55 秒（キャッシュなし・16 コア）。
- **ビルド出力の所在はソースツリーの形に従う**: fetch 分岐なら一時ツリー内、
  beside-script 分岐ならそのリポジトリ内。実行者の HOME やシステム領域に流出しない。

### 2.4 段階 3 — PATH インストール

`cargo install --path`（2 バイナリ）:

| バイナリ | 出所クレート | 実測サイズ | インストール先 |
|---|---|---|---|
| `athena` | `crates/athena-cli` | 870,808 B | `~/.cargo/bin/athena` |
| `athena-api` | `crates/athena-api` | 9,191,504 B | `~/.cargo/bin/athena-api` |

- 置き換え時は cargo の標準メッセージ（`Replacing ... Replaced package ...`）が出る。
- **プロジェクト側には何も残さない**: 実行 cwd（顔 A の空 cwd、顔 B のクローン、いずれも）
  に `.athena/`・設定・データは一切作られない（tool-only 契約、R-OPS-01/02/03 の実現）。

### 2.5 段階 4 — 完了メッセージと自動クリーンアップ

- fetch 分岐の**一時ワークツリーはトラップで自動削除される**: 実行直後の `/tmp/tmp.*` を
  全検索して**残骸 0** を確認。実行者の tmp にソースもビルド出力も残らない。
- beside-script 分岐ではクローン自身が残る（fetch 側と違って**消えない**）— 顔 B が
  クローン構成の観察に使える理由。ここに target/ が残る（§2.3）。
- 最後のメッセージは使用開始手順（次節）。

---

## 3. インストーラが次に指示すること（= インストーラの生むプロジェクト側構成）

`install.sh` 自身は**プロジェクト側の構成は作らない**（tool-only）。作るのは
インストーラが表示する次ステップの実行結果であり、それを顔 C/D で観察した:

```text
<PROJECT_ROOT>/
+-- .athena/                          # athena init が作る ATHENA_HOME
|     +-- config.yaml                 # 既定値入り。3 つの値だけ利用者が埋める
|     +-- agents/producer/{loop.md, core-requests.md}
|     +-- personas/{producer,director,lead,worker}.md   # 公開リポジトリと同一ハッシュ
|     +-- skills/                     # 空（既定 skill なし）
|     +-- .gitignore                  # 内容: var/
|     +-- var/                        # 実行時データ（ATHENA のみが書く）
|           +-- update-manifest.json  # init が出した出荷定義の証跡（files: {}）
|           +-- agent-cli-template/   # Instance の .agent-cli/ を生成する原本
|                 +-- config.toml     # 共通部分
|                 +-- commands/{ai,next_ai,close_ai,setup_ai,update_ai}.md
|                 +-- .aiprj/         # 第 2 展開場所の aiprj インベントリ（§4.2）
+-- .aiprj/                           # プロジェクト規約 + 3 文書の置き場所
|     +-- instructions.md             # 「Write instructions here」が初期値
|     +-- rules/{exec_job,close_ai,setup_project,update_project}.md
+-- .agent-cli/                       # install.sh 相当の直使用オーバーレイ
|     +-- config.toml                 # 絶対パス読みに書き換え済み（§4.1）
|     +-- commands/{ai,...,update_ai}.md
+-- .gitignore                        # .aiprj + 3 文書名（AIPRJ 規約の除外ブロック）
```

- `athena init` の出力: 「created **44** element(s), kept **0** existing element(s)」。
- 冪等性の確認: 同じプロジェクトで再実行 → 「nothing to add (**42** existing element(s) kept)」、
  追加 0・上書き 0（`ATH-AIPRJ-006` / never-overwrite の実現）。

## 4. 観察で見つかった不一致・懸念（抜け・漏れなしを確認した項目を含む）

すべて「**観察事実**」であり、修正の適否は本書内では判断しない（設計 §2.11 の
read-for-product-surfaces 原則。修正は別の変更単位で行う指示に従う）。

### 4.1 規約の読み取り指示が両面で正しく機能している（OK 項目）

| 面 | 観察 |
|---|---|
| プロジェクトルートの `.agent-cli/config.toml` | `@.aiprj/rules/exec_job.md`（cwd 相対形、ユーザー直使用用） |
| テンプレート原本の同一ファイル | `@<PROJECT_ROOT>/.aiprj/rules/exec_job.md`（絶対パスに書き換え済み。Instance 生成の原点として cwd に依らない） |

両面が設計 §21.2/§21.3 の意図通りの形になっている。テンプレート内の `.aiprj/instructions.md`
とルート側の同種ファイルは同一の初期値で、差異の原因にはなっていない。

### 4.2 インストーラが生む構成面での不一致（指摘事項）

| # | 重要度 | 指摘 | 根拠（観察事実） |
|---|---|---|---|
| F-1 | 低 | **`athena init` の要素数カウントが実生成数と合わない** | 「created 44 elements / kept 0」→ 実生成ツリーは **46 要素**（§2.3 の列挙）。再実行時の「42 existing kept」も実在 46 と不一致。生成物自体は規範形状どおり完全（欠落なし・上書きなし）。**生成の正しさには影響せず、init の自己報告の正確性のみの問題** |
| F-2 | 低 | **ルート `.gitignore` に `.aiprj` 行が重複** | 生成された root `.gitignore` は 6 行で、`.aiprj`（1 行目）がベンダー付属ブロック（5 行: `.aiprj` から始まる）と**二重に出現**。git 動作には無影響（同一パターンの重複）。AIPRJ 規約の「同じ行を 2 度書かない」整形期待とのズレ |
| F-3 | 低 | **テンプレート原本が spec §4.3.4 の図より大きい（`.aiprj/` を含む）** | spec の原本ツリー図は `config.toml` + `commands/` のみを想定。実物はさらに `.aiprj/{instructions.md, rules/}` を含む（R-AIP-05 の第 2 展開場所として**正当な**内容）。**図と実物の同期のズレ**（図側を更新するか明文化するかは別変更単位の作業） |
| F-4 | 低 | **常駐 `athena-api` の pid 記録（`api.pid`）が本フローのどこにも作られない** | 起動中・停止後とも、`<PROJECT_ROOT>/api.pid`・`.athena/api.pid`・`.athena/var/api.pid` のいずれにも pid ファイルが存在しない。実リポジトリには 2026-10-18 当時の `api.pid` が残存していた事実（X-12(b)）と併せ、**pid 記録の書き込み先がバージョン間で非一貫**の可能性がある。書き込み先の規範（R-OPS-02 の var/ 収め）とコードのずれを確認する価値がある |

### 4.3 冪等性・安全性の確認結果（問題なしと確認した項目）

| 確認 | 実測 |
|---|---|
| ビルドの再実行（A2/B2） | 両面とも終了 0、置き換えメッセージのみ、追加生成なし |
| 一時ワークツリーの除去（顔 A） | `/tmp/tmp.*` 残骸 0（トラップ確認済み） |
| プロジェクト側への流出（tool-only 契約） | 顔 A/B とも実行 cwd に `.athena/`・設定・データ 0 |
| **`athena init` の never-overwrite** | 既存 42 要素のまま保持（再実行で上書き 0、追加 0） |
| **`athena init` の要素の正しさ** | 4 persona は公開リポジトリと同一ハッシュ、config.yaml は §28.1 既定形 |

---

## 5. 結論

新規インストール時（`./install.sh` 実行時）のディレクトリ構成は、**公開リポジトリの
設計（§2.5 / §2.3 / §21.3 / R-OPS-01…04）どおりに動作する**。

1. **ツールのインストールは HOME/システム領域のみ**（`~/.cargo/bin/` + 一時ワークツリー +
   cargo のレジストリ/ビルドの一時領域）。プロジェクト側には一切触れず、
   一時ワークツリーは自分で掃除する（fetch 分岐）。
2. **プロジェクト側の構成は全て次ステップが作る**（`athena init` → `.athena/`,
   `.aiprj/`, `.agent-cli/`; 常駐起動 → `var/` 下の実行時データ）。
   初期化は冪等で、生成物の形状・内容は公開リポジトリ（配布既定）と一致する。
3. 指摘は 4 件（§4.2）、**いずれも低重要度**（自己報告の正確性、整形、図との同期、
   pid 記録の書き込み先の確認事項）。生成物の正しさ・安全性・冪等性には影響しない。

---

## 6. 検証に用いた実測データ（要約）

| 段階 | 値 |
|---|---|
| ソース取得（フェッチ分岐・クローン実行） | `git clone --depth 1`（浅い履歴）。クローン root の全体リストは §2.2 |
| release build（初回） | 34.55 s（16 コア）; target/ = 647 MB（顔 B クローン側の実物）; release 下に最終バイナリ |
| PATH install | `athena` 870,808 B / `athena-api` 9,191,504 B → `~/.cargo/bin/` |
| init 出力 | 「created 44 element(s), kept 0 existing element(s)」（1 回目）、再実行「nothing to add (42 existing kept)」 |
| 生成要素数（実測） | 46（files + dirs の全リスト §2.3） |
| persona 同一性 | 4 ファイルとも公開リポジトリ側と同一ハッシュ |
| 常駐起動 | `athena-api` が `127.0.0.1:8080` で受付、healthz 200、Core Agent 常駐、JSON 構造化ログ |
| shutdown | SIGTERM で「stopped」まで進行。停止時のインスタンス停止+永続化を確認 |
| 常駐時の var 生成 | athena.db（3.6 MB）・core-agent Workspace（書き込み領域のみ）・update-manifest.json |

## 方法上の限定

- 顔 A の実行を**バックグラウンド観察**しようとした 2 回は、この実行環境の
  バックグラウンド実行の制約で失敗した（プロセスは開始直後に終了、ログは空のまま）。
  本書の顔 A の結果は**同期実行・直接観察**による。バックグラウンド観察は使わず、
  段階ごとの直接観察で全情報を取得した。
- 先行プローブ（16:35 開始のもの、本セッション外）によりバイナリ同一の前提は保たれて
  いるが、**初回 PATH 配置（無かった場合）の挙動**は直接観察していない（置き換えの形の
  み実測）。パス・所有・メッセージは同一手順であり、結論への影響は構成の所在のみならず
  変わらないと判断する。
