# Serial Monitor Essential
Windows用のシリアルモニタです．他のソフトもそれぞれ使いやすい点はありますが，欲しい機能が揃ったものが見つからなかったので自作しました．

![](/README/image/window.gif)

|| 再接続  | 送信機能  |  データレート |
| ---- | ---- | ---- | ---- |
| Arduino IDE  | × | ○ | ○ |
| TeraTerm  | ○ | × | ○ |
| Serial Monitor (VS Code Extension)  | ○ | ○ | × |
| **Serial Monitor Essential**  | **○** | **○** | **○** |

* Arduino IDEは再接続に手間がかかる
* TeraTermは送信用のテキストボックスがない
* VS CodeのSerial Monitorは多くのデータを受信すると詰まる(≠baudrate)

Serial Monitor Essentialでは他にも様々な機能を実装しています．
* 送信時の改行コードの選択
* 上下矢印キーによる送信履歴の表示
* 受信データの折り返しの切替え
* 改行コードの表示の切替え
* NULL文字の表示の切替え
* 自動スクロールの切替え
* タイムスタンプの切替え
* 受信データのクリップボードへのコピー，ファイルへの保存

# How to Install
1. [Release](https://github.com/771-8bit/SerialMonitorEssential/releases)から```SetupSerialMonitorEssential.zip```をダウンロードし，任意のディレクトリに展開．
![](/README/image/download.png)

2. ```setup.exe```を実行．
![](/README/image/setup.png)

3. セットアップウィザードの通りに進める．
![](/README/image/SetupWizard.png) 

4. インストール完了．プログラムメニューに登録される．
![](/README/image/ProgramMenu.png)

