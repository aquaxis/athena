# `agent-cli` 0.53.0 検証記録(`ATH-DEV-022` の再検証、2026-11-14)

| 項目 | 内容 |
|---|---|
| 基盤 | `ATH-DEV-022` の検証は **0.29.0** に対して行われていた(`docs/agent_cli_0290_findings_ja.md`)。導入バイナリの実測が **0.53.0** であったため、チェックリストを再実行した。probe は `/var/tmp` 配下で scratch な `HOME` を用い、運用者自身の設定には一切触れていない — 0.29.0 の F-5 が、legacy キーを持つユーザー設定ファイルを `agent-cli` がその場で書き換えることを確認しているため。 |
| 主記録 | 要件 §3.34.64、spec 改訂履歴の 2026-11-14 行。0.29.0 の記録は履歴として保持(D-29) |
| 英語版 | `docs/agent_cli_0530_findings.md`(同一内容) |

## 1. 0.29.0 のチェックリストを再実行

| 検査 | 0.53.0 での観測 |
|---|---|
| 設定チェーン | **不変**。`AGENT_CLI_CONFIG` → `~/.config/agent-cli/config.toml` に `./.agent-cli/config.toml` を重ねる — `--help` に明記され、全 probe で実際に用いた。§2.2 の前提は成立 |
| `serve`(前景 headless) | 起動して peer に応答。**panic なし**。非適合 persona はハングではなく明確なエラーで拒否される(F-1 参照) |
| `spawn`(detached) | **stdout 契約は同一**: `spawned detached agent: id=<agent-id> name=<name> pid=<os pid> socket=<path>`。launcher は exit 0、子は launcher より長命、peer として登録される。この行から peer agent-id を取り出す D-18 は 0.53.0 でも安全 |
| IPC `ask` / `list` / `stop` | 往復・一覧・graceful stop と登録解除をすべて実 peer に対して確認。**`ask` の引数形が変更**: `agent-cli ask --persona <PERSONA> <PEER> <TEXT>` — peer と本文が位置引数で、`--persona` が必須 |
| 文脈保持 | **成立**。ある `ask` で述べた事実(`ZEBRA-7`)を、同一常駐 peer に対する別の `ask` が想起した |
| 手順ファイルの IPC 読み | **成立**。絶対パスのファイル読みは内容を逐語で返し、`/update_ai` は**リテラルのテキスト**として到達し展開されない — `ATH-ACLI-016` / `ATH-ACLI-017` を再確認 |
| 設定キー | 既に canonical な綴りのファイルは呼び出しを跨いで **byte 同一**(md5 で確認)であり、制御面チェックサム(`ATH-CORE-044`)は安全。legacy rename の**通知**は legacy キーを持たないファイルに対しても発火するが、**書き換えは起きない**(通知のみ) |
| 長命常駐 | 入力を与えずに spawn した peer は 46 秒時点で常駐。`ask` を 2 回処理した後も、最後の入力から **83 秒**時点で常駐していた。`--help` は `spawn` が起動する peer に `--exit-when-idle` を既定で付けると述べているが、その終了は**観測されなかった**。§2.2 / §7.5 / §14.4 の長命セッション前提は成立 — ただし数値は 83 秒の観測であり、無期限常駐の証明ではない |

## 2. 発見された乖離

| # | 乖離 | 帰結と処置 |
|---|---|---|
| F-1 | **persona の front matter に `role` が必須**。0.29.0 の記録は front matter の存在のみを要求していたが、0.53.0 は `role` を欠く persona を拒否する(`persona error: invalid YAML frontmatter: missing field role`)。その persona を `spawn` すると、launcher 側には `detached agent did not register within 5s` としてしか現れない | **ATHENA は適合**: 出荷 persona はすべて `name` / `role` / `level` / `allowed_tools` を宣言している。ATHENA 自身の spawn が成功する一方で最小構成の probe persona が失敗した理由はこれである。次の読み手が launcher のタイムアウト文言を退行と誤読しないよう記録する |
| F-2 | **`[provider]` 直下の `model` は無視される**。`[provider] kind = "ollama"` ＋ `model = "glm-5.3:cloud"`(ATHENA の生成形そのまま)で peer は provider 内蔵既定の `glm-5.1:cloud` で動作した。`[provider.<kind>] model` は**有効**で、`--model` フラグも有効 | **ATHENA 側の欠陥であり、本変更単位で修復**。生成した overlay が運用者の明示したモデルを黙って捨てていた — ATHENA はモデルの明示を検証しておきながら、効果を持たないファイルを出荷していたことになり、これは `ATH-AGENT-008` が防ぐべき帰結そのものである。overlay は `[provider.<kind>] model` を書くようになり、provider 名が bare key でない場合(`llama.cpp`)はテーブル名を引用する。spec §2.2.1 の `ATH-ACLI-012` 対応表も改訂した(`ATH-DEV-022` 自身の指示) |
| F-3 | **`ask` の引数形の変更**(位置引数 `<PEER> <TEXT>`、`--persona` 必須) | ATHENA は `ask` を shell 越しに呼ばず、transport が IPC で直接 peer を指すため ATHENA 側の変更は不要。0.29.0 記録の probe コマンドがそのままでは動かないため記録する |
| F-4 | `--help` は `spawn` が `--exit-when-idle` を既定で付けると述べるが、spawn した peer は最後の入力から 83 秒時点でも常駐していた | ATHENA 側の変更なし。`agent-cli` 側の文書と挙動の不一致として**観測のみ**を記録し、どちら向きにも依存しない: ATHENA の監督は離脱した instance #0 を既に復旧対象として扱う(`ATH-CORE-029`、X-62) |

## 3. 0.29.0 の追跡項目を再計測

| 項目 | 結果 |
|---|---|
| F-4(0.29.0) — cold boot での初回 spawn 死 | **再現しなかった**。適合 persona による 4 回の spawn はすべて初回で登録され、ATHENA 自身の常駐もセッション 067〜072 を通じて instance #0 を初回で起動している。0.29.0 が置いた基準(初回 spawn が再起動なしで常駐する cold boot)は本ホストで満たされた。上流で修正されたとは主張しない — ここで再現しなかっただけである |
| `kill -9` 後のレジストリ掃除 | **改善**。0.29.0 は probe するまで `list` に滞留エントリを残したが、0.53.0 では `SIGKILL` 後にエントリが**掃除される**(実測: kill 後 5 秒で listed 数 0)。D-18 の agent-id 宛て指定は依然正しく、費用もない |
| ollama のウォームアップ遅延プロファイル | **未計測**。probe の `ask` はいずれもタイムアウト内に完了したが、初回ロードと定常状態の比較プロファイルは取っていないため、本セッションから `startup_timeout` の入力値は主張しない |

## 4. 本記録が主張しないこと

`docs/nfr_compliance_ja.md` の NFR 数値は 0.29.0 で取得されたものであり、**そのバージョンを保持**
する — いずれも本セッションで再計測していない。本セッションが再実行したのは `ATH-DEV-022` が
名指す**挙動**のチェックリストであり、性能計測ではない。
