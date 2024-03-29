# Serial Monitor Essential
Windows用のシリアルモニタです．他のソフトもそれぞれ使いやすい点はありますが，欲しい機能が揃ったものが見つからなかったので自作しました．

![](/README/image/window.gif)

## :sparkles: Features
|| &nbsp;&nbsp;&nbsp;&nbsp;再接続&nbsp;&nbsp;&nbsp;&nbsp;  | &nbsp;&nbsp;送信機能&nbsp;&nbsp;  |  データレート | バイナリ送受信 |
| :--- | :---: | :---: | :---: | :---: |
| Arduino IDE  | :x: | :white_check_mark: | :white_check_mark: | :x: |
| Tera Term  | :white_check_mark: | :x: | :white_check_mark: | :white_check_mark: |
| Serial Monitor (VS Code Extension)  | :white_check_mark: | :white_check_mark: | :x: | :x: |
| **Serial Monitor Essential**  | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |

* Arduino IDEは再接続に手間がかかる
* Tera Termは送信用のテキストボックスがない
* VS CodeのSerial Monitorは多くのデータを受信すると詰まる(≠baudrate)

Serial Monitor Essentialでは他にも様々な機能を実装しています．
* 送信時の改行コードの選択
* 上下矢印キーによる送信履歴の表示
* 受信データの折り返しの切替え
* 改行コードの表示の切替え
* NULL文字の表示の切替え
* 自動スクロールの切替え
* タイムスタンプの切替え
* バイナリ送受信
* 受信データのクリップボードへのコピー，ファイルへの保存
* 各種設定の保存

## :arrow_down: How to Install
1. [Release](https://github.com/771-8bit/SerialMonitorEssential/releases)から```SetupSerialMonitorEssential.zip```をダウンロードし，任意のディレクトリに展開します．

![](/README/image/download.png)

2. ```setup.exe```を実行します．

![](/README/image/setup.png)

3. セットアップウィザードの通りに進めます．

![](/README/image/SetupWizard.png) 

4. インストールが完了し，プログラムメニューに登録されます．

![](/README/image/ProgramMenu.png)

## License
ソフトウェアはMITライセンスで公開しています．

テキストボックスのフォントに[Ricty Diminished](https://rictyfonts.github.io/diminished)を使用しています．  
ライセンスは[SIL Open Font License (OFL) Version 1.1](http://scripts.sil.org/ofl)です．  
© 2017 [Yusa Yasunori](http://www.yusa.lab.uec.ac.jp/~yusa/)

