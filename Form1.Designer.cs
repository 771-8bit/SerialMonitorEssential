namespace SerialMonitorEssential
{
    partial class Form1
    {
        /// <summary>
        /// 必要なデザイナー変数です。
        /// </summary>
        private System.ComponentModel.IContainer components = null;

        /// <summary>
        /// 使用中のリソースをすべてクリーンアップします。
        /// </summary>
        /// <param name="disposing">マネージド リソースを破棄する場合は true を指定し、その他の場合は false を指定します。</param>
        protected override void Dispose(bool disposing)
        {
            if (disposing && (components != null))
            {
                components.Dispose();
            }
            base.Dispose(disposing);
        }

        #region Windows フォーム デザイナーで生成されたコード

        /// <summary>
        /// デザイナー サポートに必要なメソッドです。このメソッドの内容を
        /// コード エディターで変更しないでください。
        /// </summary>
        private void InitializeComponent()
        {
            this.components = new System.ComponentModel.Container();
            System.ComponentModel.ComponentResourceManager resources = new System.ComponentModel.ComponentResourceManager(typeof(Form1));
            this.serialPort1 = new System.IO.Ports.SerialPort(this.components);
            this.grpSetting = new System.Windows.Forms.GroupBox();
            this.groupBox1 = new System.Windows.Forms.GroupBox();
            this.cmbDataBits = new System.Windows.Forms.ComboBox();
            this.cmbStopBits = new System.Windows.Forms.ComboBox();
            this.cmbParity = new System.Windows.Forms.ComboBox();
            this.checkRtsEnable = new System.Windows.Forms.CheckBox();
            this.checkDtrEnable = new System.Windows.Forms.CheckBox();
            this.cmbHandshake = new System.Windows.Forms.ComboBox();
            this.reload_COM = new System.Windows.Forms.Button();
            this.reconnect = new System.Windows.Forms.CheckBox();
            this.cmbPortName = new System.Windows.Forms.ComboBox();
            this.cmbBaudRate = new System.Windows.Forms.ComboBox();
            this.connectButton = new System.Windows.Forms.Button();
            this.rcvTextBox = new System.Windows.Forms.RichTextBox();
            this.grpSend = new System.Windows.Forms.GroupBox();
            this.checkSendBIN = new System.Windows.Forms.CheckBox();
            this.panel2 = new System.Windows.Forms.Panel();
            this.sndTextBox = new System.Windows.Forms.TextBox();
            this.cmbCRLF = new System.Windows.Forms.ComboBox();
            this.checkEnter = new System.Windows.Forms.CheckBox();
            this.btnSend = new System.Windows.Forms.Button();
            this.grpRecv = new System.Windows.Forms.GroupBox();
            this.checkBIN = new System.Windows.Forms.CheckBox();
            this.panel3 = new System.Windows.Forms.Panel();
            this.textNote = new System.Windows.Forms.TextBox();
            this.panel1 = new System.Windows.Forms.Panel();
            this.rcvTextBoxScroll = new System.Windows.Forms.TextBox();
            this.checkWrap = new System.Windows.Forms.CheckBox();
            this.btnCopy = new System.Windows.Forms.Button();
            this.checkCRLF = new System.Windows.Forms.CheckBox();
            this.checkNULL = new System.Windows.Forms.CheckBox();
            this.cmbTimestamp = new System.Windows.Forms.ComboBox();
            this.btnSaveRcv = new System.Windows.Forms.Button();
            this.btnClearRcv = new System.Windows.Forms.Button();
            this.check_timestamp = new System.Windows.Forms.CheckBox();
            this.autoScroll = new System.Windows.Forms.CheckBox();
            this.contextMenuStrip1 = new System.Windows.Forms.ContextMenuStrip(this.components);
            this.contextMenuStrip2 = new System.Windows.Forms.ContextMenuStrip(this.components);
            this.timerReadSerial = new System.Windows.Forms.Timer(this.components);
            this.timerReconnect = new System.Windows.Forms.Timer(this.components);
            this.grpSetting.SuspendLayout();
            this.groupBox1.SuspendLayout();
            this.grpSend.SuspendLayout();
            this.panel2.SuspendLayout();
            this.grpRecv.SuspendLayout();
            this.panel3.SuspendLayout();
            this.panel1.SuspendLayout();
            this.SuspendLayout();
            // 
            // serialPort1
            // 
            this.serialPort1.BaudRate = 112500;
            this.serialPort1.ReadBufferSize = 65536;
            // 
            // grpSetting
            // 
            this.grpSetting.Anchor = ((System.Windows.Forms.AnchorStyles)(((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Left) 
            | System.Windows.Forms.AnchorStyles.Right)));
            this.grpSetting.Controls.Add(this.groupBox1);
            this.grpSetting.Controls.Add(this.reload_COM);
            this.grpSetting.Controls.Add(this.reconnect);
            this.grpSetting.Controls.Add(this.cmbPortName);
            this.grpSetting.Controls.Add(this.cmbBaudRate);
            this.grpSetting.Controls.Add(this.connectButton);
            this.grpSetting.Location = new System.Drawing.Point(28, 30);
            this.grpSetting.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.grpSetting.Name = "grpSetting";
            this.grpSetting.Padding = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.grpSetting.Size = new System.Drawing.Size(1085, 203);
            this.grpSetting.TabIndex = 0;
            this.grpSetting.TabStop = false;
            this.grpSetting.Text = "Settings";
            // 
            // groupBox1
            // 
            this.groupBox1.Anchor = ((System.Windows.Forms.AnchorStyles)(((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Left) 
            | System.Windows.Forms.AnchorStyles.Right)));
            this.groupBox1.Controls.Add(this.cmbDataBits);
            this.groupBox1.Controls.Add(this.cmbStopBits);
            this.groupBox1.Controls.Add(this.cmbParity);
            this.groupBox1.Controls.Add(this.checkRtsEnable);
            this.groupBox1.Controls.Add(this.checkDtrEnable);
            this.groupBox1.Controls.Add(this.cmbHandshake);
            this.groupBox1.Location = new System.Drawing.Point(9, 105);
            this.groupBox1.Name = "groupBox1";
            this.groupBox1.Size = new System.Drawing.Size(1067, 89);
            this.groupBox1.TabIndex = 13;
            this.groupBox1.TabStop = false;
            this.groupBox1.Text = "Advanced";
            // 
            // cmbDataBits
            // 
            this.cmbDataBits.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbDataBits.Enabled = false;
            this.cmbDataBits.FormattingEnabled = true;
            this.cmbDataBits.Items.AddRange(new object[] {
            "7",
            "8"});
            this.cmbDataBits.Location = new System.Drawing.Point(6, 37);
            this.cmbDataBits.Name = "cmbDataBits";
            this.cmbDataBits.Size = new System.Drawing.Size(62, 38);
            this.cmbDataBits.TabIndex = 10;
            // 
            // cmbStopBits
            // 
            this.cmbStopBits.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbStopBits.Enabled = false;
            this.cmbStopBits.FormattingEnabled = true;
            this.cmbStopBits.Items.AddRange(new object[] {
            "One",
            "OnePointFive",
            "Two"});
            this.cmbStopBits.Location = new System.Drawing.Point(528, 37);
            this.cmbStopBits.Name = "cmbStopBits";
            this.cmbStopBits.Size = new System.Drawing.Size(197, 38);
            this.cmbStopBits.TabIndex = 11;
            // 
            // cmbParity
            // 
            this.cmbParity.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbParity.Enabled = false;
            this.cmbParity.FormattingEnabled = true;
            this.cmbParity.Items.AddRange(new object[] {
            "None",
            "Odd\t",
            "Even",
            "Mark",
            "Space"});
            this.cmbParity.Location = new System.Drawing.Point(74, 37);
            this.cmbParity.Name = "cmbParity";
            this.cmbParity.Size = new System.Drawing.Size(114, 38);
            this.cmbParity.TabIndex = 9;
            // 
            // checkRtsEnable
            // 
            this.checkRtsEnable.AutoSize = true;
            this.checkRtsEnable.Enabled = false;
            this.checkRtsEnable.Location = new System.Drawing.Point(361, 39);
            this.checkRtsEnable.Name = "checkRtsEnable";
            this.checkRtsEnable.Size = new System.Drawing.Size(161, 34);
            this.checkRtsEnable.TabIndex = 8;
            this.checkRtsEnable.Text = "RtsEnable";
            this.checkRtsEnable.UseVisualStyleBackColor = true;
            this.checkRtsEnable.CheckedChanged += new System.EventHandler(this.checkRtsEnable_CheckedChanged);
            // 
            // checkDtrEnable
            // 
            this.checkDtrEnable.AutoSize = true;
            this.checkDtrEnable.Checked = true;
            this.checkDtrEnable.CheckState = System.Windows.Forms.CheckState.Checked;
            this.checkDtrEnable.Enabled = false;
            this.checkDtrEnable.Location = new System.Drawing.Point(194, 39);
            this.checkDtrEnable.Name = "checkDtrEnable";
            this.checkDtrEnable.Size = new System.Drawing.Size(161, 34);
            this.checkDtrEnable.TabIndex = 7;
            this.checkDtrEnable.Text = "DtrEnable";
            this.checkDtrEnable.UseVisualStyleBackColor = true;
            this.checkDtrEnable.CheckedChanged += new System.EventHandler(this.checkDtrEnable_CheckedChanged);
            // 
            // cmbHandshake
            // 
            this.cmbHandshake.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbHandshake.Enabled = false;
            this.cmbHandshake.FormattingEnabled = true;
            this.cmbHandshake.Items.AddRange(new object[] {
            "None",
            "XOnXOff",
            "RequestToSend",
            "RequestToSendXOnXOff"});
            this.cmbHandshake.Location = new System.Drawing.Point(734, 37);
            this.cmbHandshake.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.cmbHandshake.Name = "cmbHandshake";
            this.cmbHandshake.Size = new System.Drawing.Size(322, 38);
            this.cmbHandshake.TabIndex = 0;
            // 
            // reload_COM
            // 
            this.reload_COM.Location = new System.Drawing.Point(12, 47);
            this.reload_COM.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.reload_COM.Name = "reload_COM";
            this.reload_COM.Size = new System.Drawing.Size(115, 46);
            this.reload_COM.TabIndex = 5;
            this.reload_COM.Text = "Reload";
            this.reload_COM.UseVisualStyleBackColor = true;
            this.reload_COM.Click += new System.EventHandler(this.reload_COM_Click);
            // 
            // reconnect
            // 
            this.reconnect.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Right)));
            this.reconnect.AutoSize = true;
            this.reconnect.Location = new System.Drawing.Point(871, 54);
            this.reconnect.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.reconnect.Name = "reconnect";
            this.reconnect.Size = new System.Drawing.Size(202, 34);
            this.reconnect.TabIndex = 4;
            this.reconnect.Text = "Auto Connect";
            this.reconnect.UseVisualStyleBackColor = true;
            // 
            // cmbPortName
            // 
            this.cmbPortName.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbPortName.FormattingEnabled = true;
            this.cmbPortName.ImeMode = System.Windows.Forms.ImeMode.NoControl;
            this.cmbPortName.Location = new System.Drawing.Point(139, 52);
            this.cmbPortName.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.cmbPortName.Name = "cmbPortName";
            this.cmbPortName.Size = new System.Drawing.Size(136, 38);
            this.cmbPortName.TabIndex = 2;
            // 
            // cmbBaudRate
            // 
            this.cmbBaudRate.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbBaudRate.FormattingEnabled = true;
            this.cmbBaudRate.Items.AddRange(new object[] {
            "9600",
            "19200",
            "38400",
            "57600",
            "78800",
            "115200",
            "230400",
            "460800",
            "921600",
            "1843200"});
            this.cmbBaudRate.Location = new System.Drawing.Point(287, 52);
            this.cmbBaudRate.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.cmbBaudRate.Name = "cmbBaudRate";
            this.cmbBaudRate.Size = new System.Drawing.Size(150, 38);
            this.cmbBaudRate.TabIndex = 1;
            // 
            // connectButton
            // 
            this.connectButton.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Right)));
            this.connectButton.Location = new System.Drawing.Point(683, 47);
            this.connectButton.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.connectButton.Name = "connectButton";
            this.connectButton.Size = new System.Drawing.Size(176, 46);
            this.connectButton.TabIndex = 3;
            this.connectButton.Text = "Connect";
            this.connectButton.UseVisualStyleBackColor = true;
            this.connectButton.Click += new System.EventHandler(this.connectButton_Click);
            // 
            // rcvTextBox
            // 
            this.rcvTextBox.Dock = System.Windows.Forms.DockStyle.Fill;
            this.rcvTextBox.Location = new System.Drawing.Point(0, 0);
            this.rcvTextBox.Name = "rcvTextBox";
            this.rcvTextBox.ReadOnly = true;
            this.rcvTextBox.Size = new System.Drawing.Size(1067, 337);
            this.rcvTextBox.TabIndex = 11;
            this.rcvTextBox.Text = "";
            this.rcvTextBox.WordWrap = false;
            // 
            // grpSend
            // 
            this.grpSend.Anchor = ((System.Windows.Forms.AnchorStyles)(((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Left) 
            | System.Windows.Forms.AnchorStyles.Right)));
            this.grpSend.Controls.Add(this.checkSendBIN);
            this.grpSend.Controls.Add(this.panel2);
            this.grpSend.Controls.Add(this.cmbCRLF);
            this.grpSend.Controls.Add(this.checkEnter);
            this.grpSend.Controls.Add(this.btnSend);
            this.grpSend.Location = new System.Drawing.Point(28, 249);
            this.grpSend.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.grpSend.Name = "grpSend";
            this.grpSend.Padding = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.grpSend.Size = new System.Drawing.Size(1085, 256);
            this.grpSend.TabIndex = 1;
            this.grpSend.TabStop = false;
            this.grpSend.Text = "Send";
            // 
            // checkSendBIN
            // 
            this.checkSendBIN.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Right)));
            this.checkSendBIN.AutoSize = true;
            this.checkSendBIN.Checked = true;
            this.checkSendBIN.CheckState = System.Windows.Forms.CheckState.Checked;
            this.checkSendBIN.Location = new System.Drawing.Point(835, 157);
            this.checkSendBIN.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.checkSendBIN.Name = "checkSendBIN";
            this.checkSendBIN.Size = new System.Drawing.Size(187, 34);
            this.checkSendBIN.TabIndex = 9;
            this.checkSendBIN.Text = "Send Binary";
            this.checkSendBIN.UseVisualStyleBackColor = true;
            this.checkSendBIN.CheckedChanged += new System.EventHandler(this.checkSendBIN_CheckedChanged);
            // 
            // panel2
            // 
            this.panel2.Anchor = ((System.Windows.Forms.AnchorStyles)((((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Bottom) 
            | System.Windows.Forms.AnchorStyles.Left) 
            | System.Windows.Forms.AnchorStyles.Right)));
            this.panel2.Controls.Add(this.sndTextBox);
            this.panel2.Location = new System.Drawing.Point(9, 42);
            this.panel2.Name = "panel2";
            this.panel2.Size = new System.Drawing.Size(814, 203);
            this.panel2.TabIndex = 8;
            // 
            // sndTextBox
            // 
            this.sndTextBox.Dock = System.Windows.Forms.DockStyle.Fill;
            this.sndTextBox.Location = new System.Drawing.Point(0, 0);
            this.sndTextBox.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.sndTextBox.Multiline = true;
            this.sndTextBox.Name = "sndTextBox";
            this.sndTextBox.Size = new System.Drawing.Size(814, 203);
            this.sndTextBox.TabIndex = 0;
            this.sndTextBox.KeyDown += new System.Windows.Forms.KeyEventHandler(this.sndTextBox_KeyDown);
            // 
            // cmbCRLF
            // 
            this.cmbCRLF.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Right)));
            this.cmbCRLF.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbCRLF.FormattingEnabled = true;
            this.cmbCRLF.Items.AddRange(new object[] {
            "None",
            "LF(\\n)",
            "CR(\\r)",
            "CRLF"});
            this.cmbCRLF.Location = new System.Drawing.Point(832, 207);
            this.cmbCRLF.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.cmbCRLF.Name = "cmbCRLF";
            this.cmbCRLF.Size = new System.Drawing.Size(231, 38);
            this.cmbCRLF.TabIndex = 7;
            // 
            // checkEnter
            // 
            this.checkEnter.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Right)));
            this.checkEnter.AutoSize = true;
            this.checkEnter.Checked = true;
            this.checkEnter.CheckState = System.Windows.Forms.CheckState.Checked;
            this.checkEnter.Location = new System.Drawing.Point(835, 109);
            this.checkEnter.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.checkEnter.Name = "checkEnter";
            this.checkEnter.Size = new System.Drawing.Size(233, 34);
            this.checkEnter.TabIndex = 2;
            this.checkEnter.Text = "Send with Enter";
            this.checkEnter.UseVisualStyleBackColor = true;
            this.checkEnter.CheckedChanged += new System.EventHandler(this.checkEnter_CheckedChanged);
            // 
            // btnSend
            // 
            this.btnSend.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Right)));
            this.btnSend.Location = new System.Drawing.Point(832, 47);
            this.btnSend.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.btnSend.Name = "btnSend";
            this.btnSend.Size = new System.Drawing.Size(231, 46);
            this.btnSend.TabIndex = 1;
            this.btnSend.Text = "Send";
            this.btnSend.UseVisualStyleBackColor = true;
            this.btnSend.Click += new System.EventHandler(this.btnSend_Click);
            // 
            // grpRecv
            // 
            this.grpRecv.Anchor = ((System.Windows.Forms.AnchorStyles)((((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Bottom) 
            | System.Windows.Forms.AnchorStyles.Left) 
            | System.Windows.Forms.AnchorStyles.Right)));
            this.grpRecv.Controls.Add(this.checkBIN);
            this.grpRecv.Controls.Add(this.panel3);
            this.grpRecv.Controls.Add(this.panel1);
            this.grpRecv.Controls.Add(this.checkWrap);
            this.grpRecv.Controls.Add(this.btnCopy);
            this.grpRecv.Controls.Add(this.checkCRLF);
            this.grpRecv.Controls.Add(this.checkNULL);
            this.grpRecv.Controls.Add(this.cmbTimestamp);
            this.grpRecv.Controls.Add(this.btnSaveRcv);
            this.grpRecv.Controls.Add(this.btnClearRcv);
            this.grpRecv.Controls.Add(this.check_timestamp);
            this.grpRecv.Controls.Add(this.autoScroll);
            this.grpRecv.Location = new System.Drawing.Point(28, 521);
            this.grpRecv.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.grpRecv.Name = "grpRecv";
            this.grpRecv.Padding = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.grpRecv.Size = new System.Drawing.Size(1085, 527);
            this.grpRecv.TabIndex = 2;
            this.grpRecv.TabStop = false;
            this.grpRecv.Text = "Recieve";
            // 
            // checkBIN
            // 
            this.checkBIN.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Left)));
            this.checkBIN.AutoSize = true;
            this.checkBIN.Location = new System.Drawing.Point(229, 433);
            this.checkBIN.Name = "checkBIN";
            this.checkBIN.Size = new System.Drawing.Size(192, 34);
            this.checkBIN.TabIndex = 18;
            this.checkBIN.Text = "Binary (hex)";
            this.checkBIN.UseVisualStyleBackColor = true;
            this.checkBIN.CheckedChanged += new System.EventHandler(this.checkBIN_CheckedChanged);
            // 
            // panel3
            // 
            this.panel3.Anchor = ((System.Windows.Forms.AnchorStyles)(((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Left) 
            | System.Windows.Forms.AnchorStyles.Right)));
            this.panel3.Controls.Add(this.textNote);
            this.panel3.Location = new System.Drawing.Point(9, 42);
            this.panel3.Name = "panel3";
            this.panel3.Size = new System.Drawing.Size(1067, 41);
            this.panel3.TabIndex = 17;
            // 
            // textNote
            // 
            this.textNote.AcceptsTab = true;
            this.textNote.Dock = System.Windows.Forms.DockStyle.Fill;
            this.textNote.Location = new System.Drawing.Point(0, 0);
            this.textNote.Multiline = true;
            this.textNote.Name = "textNote";
            this.textNote.Size = new System.Drawing.Size(1067, 41);
            this.textNote.TabIndex = 16;
            // 
            // panel1
            // 
            this.panel1.Anchor = ((System.Windows.Forms.AnchorStyles)((((System.Windows.Forms.AnchorStyles.Top | System.Windows.Forms.AnchorStyles.Bottom) 
            | System.Windows.Forms.AnchorStyles.Left) 
            | System.Windows.Forms.AnchorStyles.Right)));
            this.panel1.Controls.Add(this.rcvTextBox);
            this.panel1.Controls.Add(this.rcvTextBoxScroll);
            this.panel1.Location = new System.Drawing.Point(9, 89);
            this.panel1.Name = "panel1";
            this.panel1.Size = new System.Drawing.Size(1067, 337);
            this.panel1.TabIndex = 15;
            // 
            // rcvTextBoxScroll
            // 
            this.rcvTextBoxScroll.Dock = System.Windows.Forms.DockStyle.Fill;
            this.rcvTextBoxScroll.Location = new System.Drawing.Point(0, 0);
            this.rcvTextBoxScroll.Multiline = true;
            this.rcvTextBoxScroll.Name = "rcvTextBoxScroll";
            this.rcvTextBoxScroll.ReadOnly = true;
            this.rcvTextBoxScroll.ScrollBars = System.Windows.Forms.ScrollBars.Both;
            this.rcvTextBoxScroll.Size = new System.Drawing.Size(1067, 337);
            this.rcvTextBoxScroll.TabIndex = 12;
            this.rcvTextBoxScroll.WordWrap = false;
            // 
            // checkWrap
            // 
            this.checkWrap.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Left)));
            this.checkWrap.AutoSize = true;
            this.checkWrap.Location = new System.Drawing.Point(12, 432);
            this.checkWrap.Name = "checkWrap";
            this.checkWrap.Size = new System.Drawing.Size(163, 34);
            this.checkWrap.TabIndex = 14;
            this.checkWrap.Text = "Line Wrap";
            this.checkWrap.UseVisualStyleBackColor = true;
            this.checkWrap.CheckedChanged += new System.EventHandler(this.checkWrap_CheckedChanged);
            // 
            // btnCopy
            // 
            this.btnCopy.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Right)));
            this.btnCopy.Location = new System.Drawing.Point(765, 467);
            this.btnCopy.Margin = new System.Windows.Forms.Padding(6);
            this.btnCopy.Name = "btnCopy";
            this.btnCopy.Size = new System.Drawing.Size(150, 46);
            this.btnCopy.TabIndex = 13;
            this.btnCopy.Text = "Copy";
            this.btnCopy.UseVisualStyleBackColor = true;
            this.btnCopy.Click += new System.EventHandler(this.btnCopy_Click);
            // 
            // checkCRLF
            // 
            this.checkCRLF.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Left)));
            this.checkCRLF.AutoSize = true;
            this.checkCRLF.Location = new System.Drawing.Point(486, 432);
            this.checkCRLF.Name = "checkCRLF";
            this.checkCRLF.Size = new System.Drawing.Size(214, 34);
            this.checkCRLF.TabIndex = 10;
            this.checkCRLF.Text = "Show [CRLF]  ";
            this.checkCRLF.UseVisualStyleBackColor = true;
            // 
            // checkNULL
            // 
            this.checkNULL.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Left)));
            this.checkNULL.AutoSize = true;
            this.checkNULL.Location = new System.Drawing.Point(706, 432);
            this.checkNULL.Name = "checkNULL";
            this.checkNULL.Size = new System.Drawing.Size(203, 34);
            this.checkNULL.TabIndex = 9;
            this.checkNULL.Text = "Show [NUL]  ";
            this.checkNULL.UseVisualStyleBackColor = true;
            // 
            // cmbTimestamp
            // 
            this.cmbTimestamp.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Left)));
            this.cmbTimestamp.DropDownStyle = System.Windows.Forms.ComboBoxStyle.DropDownList;
            this.cmbTimestamp.FormattingEnabled = true;
            this.cmbTimestamp.Items.AddRange(new object[] {
            " > ",
            ","});
            this.cmbTimestamp.Location = new System.Drawing.Point(486, 473);
            this.cmbTimestamp.Name = "cmbTimestamp";
            this.cmbTimestamp.Size = new System.Drawing.Size(71, 38);
            this.cmbTimestamp.TabIndex = 5;
            // 
            // btnSaveRcv
            // 
            this.btnSaveRcv.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Right)));
            this.btnSaveRcv.Location = new System.Drawing.Point(603, 468);
            this.btnSaveRcv.Margin = new System.Windows.Forms.Padding(6);
            this.btnSaveRcv.Name = "btnSaveRcv";
            this.btnSaveRcv.Size = new System.Drawing.Size(150, 46);
            this.btnSaveRcv.TabIndex = 4;
            this.btnSaveRcv.Text = "Save";
            this.btnSaveRcv.UseVisualStyleBackColor = true;
            this.btnSaveRcv.Click += new System.EventHandler(this.btnSaveRcv_Click);
            // 
            // btnClearRcv
            // 
            this.btnClearRcv.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Right)));
            this.btnClearRcv.Location = new System.Drawing.Point(923, 467);
            this.btnClearRcv.Margin = new System.Windows.Forms.Padding(6);
            this.btnClearRcv.Name = "btnClearRcv";
            this.btnClearRcv.Size = new System.Drawing.Size(150, 46);
            this.btnClearRcv.TabIndex = 3;
            this.btnClearRcv.Text = "CLEAR";
            this.btnClearRcv.UseVisualStyleBackColor = true;
            this.btnClearRcv.Click += new System.EventHandler(this.btnClearRcv_Click);
            // 
            // check_timestamp
            // 
            this.check_timestamp.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Left)));
            this.check_timestamp.AutoSize = true;
            this.check_timestamp.Checked = true;
            this.check_timestamp.CheckState = System.Windows.Forms.CheckState.Checked;
            this.check_timestamp.Location = new System.Drawing.Point(229, 475);
            this.check_timestamp.Margin = new System.Windows.Forms.Padding(6);
            this.check_timestamp.Name = "check_timestamp";
            this.check_timestamp.Size = new System.Drawing.Size(248, 34);
            this.check_timestamp.TabIndex = 2;
            this.check_timestamp.Text = "Show Timestamp";
            this.check_timestamp.UseVisualStyleBackColor = true;
            this.check_timestamp.CheckedChanged += new System.EventHandler(this.check_timestamp_CheckedChanged);
            // 
            // autoScroll
            // 
            this.autoScroll.Anchor = ((System.Windows.Forms.AnchorStyles)((System.Windows.Forms.AnchorStyles.Bottom | System.Windows.Forms.AnchorStyles.Left)));
            this.autoScroll.AutoSize = true;
            this.autoScroll.Checked = true;
            this.autoScroll.CheckState = System.Windows.Forms.CheckState.Checked;
            this.autoScroll.Location = new System.Drawing.Point(12, 475);
            this.autoScroll.Margin = new System.Windows.Forms.Padding(6);
            this.autoScroll.Name = "autoScroll";
            this.autoScroll.Size = new System.Drawing.Size(170, 34);
            this.autoScroll.TabIndex = 1;
            this.autoScroll.Text = "Auto Scroll";
            this.autoScroll.UseVisualStyleBackColor = true;
            this.autoScroll.CheckedChanged += new System.EventHandler(this.autoScroll_CheckedChanged);
            // 
            // contextMenuStrip1
            // 
            this.contextMenuStrip1.ImageScalingSize = new System.Drawing.Size(32, 32);
            this.contextMenuStrip1.Name = "contextMenuStrip1";
            this.contextMenuStrip1.Size = new System.Drawing.Size(61, 4);
            // 
            // contextMenuStrip2
            // 
            this.contextMenuStrip2.ImageScalingSize = new System.Drawing.Size(32, 32);
            this.contextMenuStrip2.Name = "contextMenuStrip2";
            this.contextMenuStrip2.Size = new System.Drawing.Size(61, 4);
            // 
            // timerReadSerial
            // 
            this.timerReadSerial.Enabled = true;
            this.timerReadSerial.Interval = 20;
            this.timerReadSerial.Tick += new System.EventHandler(this.timerReadSerial_Tick);
            // 
            // timerReconnect
            // 
            this.timerReconnect.Enabled = true;
            this.timerReconnect.Interval = 200;
            this.timerReconnect.Tick += new System.EventHandler(this.timerReconnect_Tick);
            // 
            // Form1
            // 
            this.AutoScaleDimensions = new System.Drawing.SizeF(192F, 192F);
            this.AutoScaleMode = System.Windows.Forms.AutoScaleMode.Dpi;
            this.BackColor = System.Drawing.SystemColors.Window;
            this.ClientSize = new System.Drawing.Size(1141, 1096);
            this.Controls.Add(this.grpRecv);
            this.Controls.Add(this.grpSend);
            this.Controls.Add(this.grpSetting);
            this.Font = new System.Drawing.Font("Meiryo UI", 9F, System.Drawing.FontStyle.Regular, System.Drawing.GraphicsUnit.Point, ((byte)(128)));
            this.Icon = ((System.Drawing.Icon)(resources.GetObject("$this.Icon")));
            this.Margin = new System.Windows.Forms.Padding(6, 8, 6, 8);
            this.MinimumSize = new System.Drawing.Size(1167, 807);
            this.Name = "Form1";
            this.StartPosition = System.Windows.Forms.FormStartPosition.CenterScreen;
            this.Text = "Serial Monitor Essential";
            this.FormClosing += new System.Windows.Forms.FormClosingEventHandler(this.Form1_FormClosing);
            this.FormClosed += new System.Windows.Forms.FormClosedEventHandler(this.Form1_FormClosed);
            this.Load += new System.EventHandler(this.Form1_Load);
            this.ResizeBegin += new System.EventHandler(this.Form1_ResizeBegin);
            this.ResizeEnd += new System.EventHandler(this.Form1_ResizeEnd);
            this.grpSetting.ResumeLayout(false);
            this.grpSetting.PerformLayout();
            this.groupBox1.ResumeLayout(false);
            this.groupBox1.PerformLayout();
            this.grpSend.ResumeLayout(false);
            this.grpSend.PerformLayout();
            this.panel2.ResumeLayout(false);
            this.panel2.PerformLayout();
            this.grpRecv.ResumeLayout(false);
            this.grpRecv.PerformLayout();
            this.panel3.ResumeLayout(false);
            this.panel3.PerformLayout();
            this.panel1.ResumeLayout(false);
            this.panel1.PerformLayout();
            this.ResumeLayout(false);

        }

        #endregion

        private System.IO.Ports.SerialPort serialPort1;
        private System.Windows.Forms.GroupBox grpSetting;
        private System.Windows.Forms.ComboBox cmbPortName;
        private System.Windows.Forms.ComboBox cmbBaudRate;
        private System.Windows.Forms.ComboBox cmbHandshake;
        private System.Windows.Forms.GroupBox grpSend;
        private System.Windows.Forms.Button btnSend;
        private System.Windows.Forms.TextBox sndTextBox;
        private System.Windows.Forms.GroupBox grpRecv;
        private System.Windows.Forms.Button connectButton;
        private System.Windows.Forms.ContextMenuStrip contextMenuStrip1;
        private System.Windows.Forms.ContextMenuStrip contextMenuStrip2;
        private System.Windows.Forms.CheckBox checkEnter;
        private System.Windows.Forms.CheckBox reconnect;
        private System.Windows.Forms.Timer timerReadSerial;
        private System.Windows.Forms.Button reload_COM;
        private System.Windows.Forms.ComboBox cmbCRLF;
        private System.Windows.Forms.Button btnSaveRcv;
        private System.Windows.Forms.Button btnClearRcv;
        private System.Windows.Forms.CheckBox check_timestamp;
        private System.Windows.Forms.CheckBox autoScroll;
        private System.Windows.Forms.ComboBox cmbTimestamp;
        private System.Windows.Forms.Timer timerReconnect;
        private System.Windows.Forms.CheckBox checkNULL;
        private System.Windows.Forms.CheckBox checkCRLF;
        private System.Windows.Forms.ComboBox cmbStopBits;
        private System.Windows.Forms.ComboBox cmbDataBits;
        private System.Windows.Forms.ComboBox cmbParity;
        private System.Windows.Forms.CheckBox checkRtsEnable;
        private System.Windows.Forms.CheckBox checkDtrEnable;
        private System.Windows.Forms.GroupBox groupBox1;
        private System.Windows.Forms.RichTextBox rcvTextBox;
        private System.Windows.Forms.TextBox rcvTextBoxScroll;
        private System.Windows.Forms.Button btnCopy;
        private System.Windows.Forms.CheckBox checkWrap;
        private System.Windows.Forms.Panel panel2;
        private System.Windows.Forms.Panel panel1;
        private System.Windows.Forms.TextBox textNote;
        private System.Windows.Forms.Panel panel3;
        private System.Windows.Forms.CheckBox checkBIN;
        private System.Windows.Forms.CheckBox checkSendBIN;
    }
}

