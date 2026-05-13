<p align="center">
  <img src="docs/screenshots/petit-mates-logo-wide.png" alt="Petit Mates" width="600">
</p>

<p align="center">
  <a href="README.md">English</a>
</p>

<p align="center">
  <strong>ウィンドウの上に住む、デスクトップの小さな仲間たち。</strong><br>
  座ったり、眠ったり、壁を登ったり、ウィンドウを渡り歩いたりする小さな爬虫類たちです。
</p>

<p align="center">
  <img src="docs/screenshots/hero.gif" alt="Petit Mates の様子" width="680">
</p>

<p align="center">
  <a href="https://github.com/rinodrops/petitmates/releases/latest">
    <img src="https://img.shields.io/github/v/release/rinodrops/petitmates?color=orange&label=ダウンロード" alt="最新リリース">
  </a>
  <img src="https://img.shields.io/badge/macOS-13%2B-blue" alt="macOS 13+">
  <img src="https://img.shields.io/badge/Windows-11-blue" alt="Windows 11">
  <img src="https://img.shields.io/badge/built%20with-Rust-orange" alt="Rust 製">
</p>

---

## キャラクター

<table>
<tr>
<td align="center" width="50%">
  <img src="docs/screenshots/char-bearded-dragon.png" alt="フトアゴヒゲトカゲ" width="180"><br>
  <strong>フトアゴヒゲトカゲ</strong><br>
  <em>好奇心旺盛な探検家。動きが速く、隅々まで調べずにはいられない。</em>
</td>
<td align="center" width="50%">
  <img src="docs/screenshots/char-pond-turtle.png" alt="クサガメ" width="180"><br>
  <strong>クサガメ</strong><br>
  <em>のんびり屋の旅人。急がないけれど、ちゃんと目的地にたどり着く。</em>
</td>
</tr>
</table>

## できること

2体のキャラクターはシステムワイドに動作します。特定のアプリの中ではなく、アプリウィンドウの上やデスクトップ全体を舞台に活動します。

| アニメーション                              | プレビュー                                         |
| ------------------------------------------- | -------------------------------------------------- |
| 画面外上から落下 → 着地してキョロキョロ観察 | ![fall-land](docs/screenshots/fall-land.gif)       |
| ウィンドウ上端を端から端へ歩く              | ![walk-top](docs/screenshots/walk-top.gif)         |
| 端からのぞき込む                            | ![peek-down](docs/screenshots/peek-down.gif)       |
| 壁を登る                                    | ![climb-wall](docs/screenshots/climb-wall.gif)     |
| コーナーから別ウィンドウへジャンプ移動      | ![window-jump](docs/screenshots/window-jump.gif)   |
| 端から驚いて落下                            | ![shocked-fall](docs/screenshots/shocked-fall.gif) |
| デスクトップの床をのんびり歩く              | ![floor-walk](docs/screenshots/floor-walk.gif)     |
| カーソルをかざすと半透明になる              | ![hover-fade](docs/screenshots/hover-fade.gif)     |
| ⌘+ドラッグでつかんで別の場所へ移動          | ![drag-drop](docs/screenshots/drag-drop.gif)       |

座ったり、横になったり、眠ったり、首を傾けたり、口を開けたり——そして気が向いたら自分で別のウィンドウへ移動します。

## 動作環境

| プラットフォーム | 要件                                                        |
| ---------------- | ----------------------------------------------------------- |
| macOS            | macOS 13 Ventura 以降（Apple Silicon + Intel ユニバーサル） |
| Windows          | Windows 11、x86-64                                          |

画面収録の権限は**不要**です。公開されているウィンドウ情報 API のみを使用します。

## インストール

### macOS

1. [Releases](https://github.com/rinodrops/petitmates/releases/latest) から **`Petit-Mates-vX.X.X-darwin-universal.dmg`** をダウンロードします。
2. DMG を開き、**Petit Mates.app** をアプリケーションフォルダにドラッグします。
3. 起動するとメニューバーにアイコン（🦎）が表示されます。

### Windows

1. [Releases](https://github.com/rinodrops/petitmates/releases/latest) から **`Petit-Mates-vX.X.X-windows-x86_64.zip`** をダウンロードします。
2. ZIP を展開し、**`Petit Mates.exe`** を実行します。タスクトレイにアイコンが表示されます。

インストーラー不要。exe ファイル単体で動作します。

## 使い方

### メニューバー / タスクトレイ

<table>
<tr>
<td align="center">
  <img src="docs/screenshots/menubar-macos.png" alt="macOS メニューバー" width="260"><br>
  <em>macOS メニューバー</em>
</td>
<td align="center">
  <img src="docs/screenshots/tray-windows.png" alt="Windows タスクトレイ" width="260"><br>
  <em>Windows タスクトレイ（右クリック）</em>
</td>
</tr>
</table>

- **キャラクターの追加 / 削除** — フトアゴヒゲトカゲまたはクサガメをスポーン・削除します。
- **About** — バージョン情報。
- **終了** — アプリを終了します。

### キャラクターの移動

| 操作         | macOS        | Windows         |
| ------------ | ------------ | --------------- |
| つかんで移動 | ⌘ + ドラッグ | Ctrl + ドラッグ |

ウィンドウ端・壁・デスクトップ床のどこにでもドロップでき、キャラクターはその場からアニメーションを続けます。

### マウスホバー

キャラクターにカーソルを重ねると不透明度 25% に薄くなり、背後のウィンドウを操作できます。

## カスタマイズ

各キャラクターは起動時に `config.toml` を読み込み、**実行中でもホットリロード**されます。ファイルを保存すると約 1 秒で反映され、再起動は不要です。

### macOS

設定ファイルはアプリバンドル内にあります：

```
Petit Mates.app/Contents/Resources/assets/bearded_dragon/config.toml
Petit Mates.app/Contents/Resources/assets/pond_turtle/config.toml
```

アプリを右クリック → **「パッケージの内容を表示」** で参照できます。

### Windows

exe と同じフォルダに設定ファイルを置くと上書き適用されます：

```
Petit Mates.exe
bearded_dragon_config.toml   ← オプション（上書き用）
pond_turtle_config.toml      ← オプション（上書き用）
```

ファイルがない場合は内蔵のデフォルト値が使用されます。

## ライセンス

MIT — 詳細は [LICENSE](LICENSE) を参照してください。

---

<p align="center">
  Rust 製 · macOS + Windows · © 2026 Rino, eMotionGraphics Inc.
</p>
