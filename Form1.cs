﻿using System;
using System.Collections;
using System.Collections.Generic;
using System.ComponentModel;
using System.Data;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.IO.Ports;
using System.Management;
using System.Linq;
using System.Reflection.Emit;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;
using static System.Net.Mime.MediaTypeNames;
using static System.Windows.Forms.VisualStyles.VisualStyleElement;
using static System.Windows.Forms.VisualStyles.VisualStyleElement.Button;
using static System.Windows.Forms.VisualStyles.VisualStyleElement.Window;
using System.Xml.Linq;
using System.Text.RegularExpressions;
using static System.Windows.Forms.VisualStyles.VisualStyleElement.Header;

//https://kana-soft.com/tech/sample_0007.htm
namespace SerialMonitorEssential
{
    public partial class Form1 : Form
    {
        private delegate void Delegate_RcvDataToTextBox(string data);
        private delegate void Delegate_AppendNoScroll(string data);
        public Form1()
        {
            InitializeComponent();
        }

        [System.Runtime.InteropServices.DllImport("gdi32.dll", ExactSpelling = true)]
        private static extern IntPtr AddFontMemResourceEx(byte[] pbFont, int cbFont, IntPtr pdv, out uint pcFonts);
        private void Form1_Load(object sender, EventArgs e)
        {
            //https://dobon.net/vb/dotnet/graphics/privatefontcollection.html
            System.Drawing.Text.PrivateFontCollection pfc = new System.Drawing.Text.PrivateFontCollection();
            byte[] fontBuf = Properties.Resources.RictyDiminished_Regular;
            IntPtr fontBufPtr = System.Runtime.InteropServices.Marshal.AllocCoTaskMem(fontBuf.Length);
            System.Runtime.InteropServices.Marshal.Copy(fontBuf, 0, fontBufPtr, fontBuf.Length);
            uint cFonts;
            AddFontMemResourceEx(fontBuf, fontBuf.Length, IntPtr.Zero, out cFonts);
            pfc.AddMemoryFont(fontBufPtr, fontBuf.Length);
            System.Runtime.InteropServices.Marshal.FreeCoTaskMem(fontBufPtr);
            System.Drawing.Font f12 = new System.Drawing.Font(pfc.Families[0], 12);
            System.Drawing.Font f16 = new System.Drawing.Font(pfc.Families[0], 16);
            pfc.Dispose();

            sndTextBox.Font = f16;
            textNote.Font = f12;
            rcvTextBox.Font = f12;
            rcvTextBoxScroll.Font = f12;

            reload_COM_Click(sender, e);

            cmbPortName.DropDownStyle = ComboBoxStyle.DropDownList;

            initSettings();

            AcceptButton = btnSend;

            setEnabled(true);
        }

        private void Form1_FormClosing(object sender, FormClosingEventArgs e)
        {
            //https://dobon.net/vb/dotnet/programing/mysettings.html
            Properties.Settings.Default.setting_BaudRate= cmbBaudRate.SelectedIndex;
            Properties.Settings.Default.setting_Handshake = cmbHandshake.SelectedIndex;
            Properties.Settings.Default.setting_Enter = checkEnter.Checked;
            Properties.Settings.Default.setting_reconnect = reconnect.Checked;
            Properties.Settings.Default.setting_timestamp = check_timestamp.Checked;
            Properties.Settings.Default.setting_autoScroll = autoScroll.Checked;
            Properties.Settings.Default.setting_timestamp_string = cmbTimestamp.SelectedIndex;
            Properties.Settings.Default.setting_NULL=checkNULL.Checked;
            Properties.Settings.Default.setting_CRLF = checkCRLF.Checked;
            Properties.Settings.Default.setting_binary = checkBIN.Checked;
            Properties.Settings.Default.setting_sendBinary = checkSendBIN.Checked;
            Properties.Settings.Default.setting_StopBits=cmbStopBits.SelectedIndex;
            Properties.Settings.Default.setting_DataBits=cmbDataBits.SelectedIndex;
            Properties.Settings.Default.setting_Parity=cmbParity.SelectedIndex;
            Properties.Settings.Default.setting_RtsEnable=checkRtsEnable.Checked;
            Properties.Settings.Default.setting_DtrEnable=checkDtrEnable.Checked;
            Properties.Settings.Default.setting_Wrap=checkWrap.Checked;
            Properties.Settings.Default.setting_sendCRLF = cmbCRLF.SelectedIndex;
            Properties.Settings.Default.setting_note = textNote.Text;

            Properties.Settings.Default.Save();

        }
        private void Form1_FormClosed(object sender, FormClosedEventArgs e)
        {
            if (serialPort1.IsOpen)
            {
                serialPort1.Close();
            }
        }

        private void connectButton_Click(object sender, EventArgs e)
        {
            if (serialPort1.IsOpen)
            {
                reconnect.Checked = false;
                serialPort1.Close();
                connectButton.Text = "Connect";
                setEnabled(true);
            }
            else
            {
                connectSerial(true);
            }
        }

        List<string> previous_send_data = new List<string>();
        int previous_send_data_index = 0;
        private void btnSend_Click(object sender, EventArgs e)
        {
            if (serialPort1.IsOpen == false)
            {
                return;
            }

            String data = sndTextBox.Text;

            //https://learn.microsoft.com/ja-jp/dotnet/api/system.io.ports.serialport.write?view=dotnet-plat-ext-7.0
            if (checkSendBIN.Checked)
            {
                //https://learn.microsoft.com/ja-jp/dotnet/csharp/programming-guide/types/how-to-convert-between-hexadecimal-strings-and-numeric-types
                string[] dataSplit = data.Split('-',' ', ',', '.', '/');
                int data_length = dataSplit.Length;
                if (dataSplit[dataSplit.Length - 1] == "")
                {
                    data_length--;
                }
                var hexdata = new byte[data_length];
                for (int i = 0; i < data_length; i++)
                {
                    try
                    {
                        byte value = Convert.ToByte(dataSplit[i], 16);
                        hexdata[i] = value;
                    }
                    catch (Exception ex)
                    {
                        MessageBox.Show(ex.Message);
                        return;
                    }
                }
                try
                {
                    serialPort1.Write(hexdata, 0, data_length);
                }
                catch (Exception ex)
                {
                    MessageBox.Show(ex.Message);
                }
            }
            else
            {
                if (string.IsNullOrEmpty(data))
                {
                    return;
                }
                try
                {
                    switch (cmbCRLF.SelectedIndex)  {
                        case 0:
                            serialPort1.Write(data);
                            break;
                        case 1:
                            serialPort1.Write(data + "\n");
                            break;
                        case 2:
                            serialPort1.Write(data + "\r");
                            break;
                        case 3:
                            serialPort1.Write(data + "\r\n");
                            break;
                        default:
                            break;
                    }
                }
                catch (Exception ex)
                {
                    MessageBox.Show(ex.Message);
        
                }
            }

            sndTextBox.Clear();

            //送信履歴保存
            //https://atmarkit.itmedia.co.jp/ait/articles/1004/08/news094.html
            if (string.IsNullOrEmpty(data.Replace("\r", "").Replace("\n", ""))==false)
            {
                bool first_data = previous_send_data.Count == 0;
                bool different_data = false;
                if (!first_data)
                {
                    different_data = previous_send_data[previous_send_data.Count - 1] != data;
                }
                bool update = first_data || different_data;

                if (update)
                {
                    previous_send_data.Add(data);
                }
            }
            previous_send_data_index = previous_send_data.Count;
            
        }

        //https://hironimo.com/prog/c-sharp/c-keypress/
        private void sndTextBox_KeyDown(object sender, KeyEventArgs e)
        {
            if (e.KeyCode == Keys.Up)
            {
                if (previous_send_data_index >= 1 && previous_send_data.Count > previous_send_data_index - 1 && previous_send_data.Count != 0)
                {
                    previous_send_data_index--;
                    sndTextBox.Clear();
                    sndTextBox.AppendText(previous_send_data[previous_send_data_index]);
                }
            }
            if (e.KeyCode == Keys.Down)
            {
                if (previous_send_data_index >= -1 && previous_send_data.Count > previous_send_data_index + 1)
                {
                    previous_send_data_index++;
                    sndTextBox.Clear();
                    sndTextBox.AppendText(previous_send_data[previous_send_data_index]);
                }
                else if (previous_send_data.Count == previous_send_data_index + 1)
                {
                    previous_send_data_index++;
                    sndTextBox.Clear();
                }
            }
        }

        StringBuilder data_queue = new System.Text.StringBuilder();
        private void RcvDataToTextBox(string data)
        {
            if (data != null)
            {
                //http://hachisue.blog65.fc2.com/blog-entry-413.html
                rcvTextBoxScroll.AppendText(data);
                if (rcvTextBox.Focused)
                {
                    data_queue.Append(data);
                }
                else
                {
                    rcvTextBox.AppendText(data_queue.ToString());
                    data_queue.Clear();
                    rcvTextBox.AppendText(data);
                }
            }

        }

        bool last_is_newline=false;
        bool first_timestamp = true;
        private void timerReadSerial_Tick(object sender, EventArgs e)
        {
            //https://atmarkit.itmedia.co.jp/ait/articles/0511/11/news117.html
            if (serialPort1.IsOpen == false)
            {
                return;
            }

            try
            {
                StringBuilder new_text = new System.Text.StringBuilder();
                bool newdata = false;
                String timestamp = String.Format("{0:HH:mm:ss.f}{1}", FastDateTime.Now, cmbTimestamp.SelectedItem.ToString());

                if (checkBIN.Checked)
                {
                    if (check_timestamp.Checked)
                    {
                        new_text.Append(timestamp);
                    }

                    while (serialPort1.BytesToRead>0)
                    {
                        var hexdata = new byte[1024];
                        int DATA_LEN_MAX = 1024;
                        int data_len = serialPort1.Read(hexdata, 0, DATA_LEN_MAX);
                        if (data_len > 0)
                        {
                            new_text.Append(BitConverter.ToString(hexdata, 0, data_len));
                            new_text.Append("-");
                            newdata = true;
                        }
                    }

                    new_text.Append("\r\n");
                }
                else
                {
                    string rawdata = serialPort1.ReadExisting();
                    if (string.IsNullOrEmpty(rawdata) == false)
                    {
                        newdata = true;

                        if ((( first_timestamp || last_is_newline )) && check_timestamp.Checked )
                        {
                            new_text.Append(timestamp);
                            first_timestamp = false;
                        }

                        string newline = "\r\n";
                        if (check_timestamp.Checked)
                        {
                            newline += timestamp;
                        }

                        new_text.Append(rawdata);

                        if (checkNULL.Checked)
                        {
                            new_text.Replace("\0", "[NULL]");
                        }
                        else
                        {
                            new_text.Replace("\0", "");
                        }

                        //https://dobon.net/vb/dotnet/string/replace.html
                        if (checkCRLF.Checked)
                        {
                            new_text.Replace("\r\n", "[CRLF]\0");
                            new_text.Replace("\r", "[CR]\0");
                            new_text.Replace("\n", "[LF]\0");
                            new_text.Replace("\0", newline);
                        }
                        else
                        {
                            new_text.Replace("\r\n", "\n");
                            new_text.Replace("\r", "\n");
                            new_text.Replace("\n", newline);
                        }

                        if (rawdata.EndsWith("\r") || rawdata.EndsWith("\n"))
                        {
                            last_is_newline = true;
                            if (check_timestamp.Checked)
                            {
                                new_text.Remove(new_text.Length - (timestamp.Length), (timestamp.Length));
                            }
                        }
                        else
                        {
                            last_is_newline = false;
                        }
                    }
                }

                if (newdata) { 
                    if (resizing)
                    {
                        resizing_buf.Append(new_text);
                    }
                    else
                    {
                        Invoke(new Delegate_RcvDataToTextBox(RcvDataToTextBox), new object[] { new_text.ToString() });
                    }
                }
            }
            catch (Exception ex)
            {
                MessageBox.Show(ex.Message);
            }

        }
        public class ItemSet
        {
            // DisplayMemberとValueMemberにはプロパティで指定する仕組み
            public String ItemDisp { get; set; }
            public String ItemValue { get; set; }

            // プロパティをコンストラクタでセット
            public ItemSet(String v, String s)
            {
                ItemDisp = s;
                ItemValue = v;
            }
        }

        List<ItemSet> src = new List<ItemSet>();
        private void reload_COM_Click(object sender, EventArgs e)
        {
            var deviceNameList = new System.Collections.ArrayList();

            int int_Max = 0;
            src.Clear();

            var check = new System.Text.RegularExpressions.Regex("(COM[1-9][0-9]?[0-9]?)");

            //https://truthfullscore.hatenablog.com/entry/2014/01/10/180608
            ManagementClass mcPnPEntity = new ManagementClass("Win32_PnPEntity");
            ManagementObjectCollection manageObjCol = mcPnPEntity.GetInstances();

            //全てのPnPデバイスを探索しシリアル通信が行われるデバイスを随時追加する
            foreach (ManagementObject manageObj in manageObjCol)
            {
                //Nameプロパティを取得
                var namePropertyValue = manageObj.GetPropertyValue("Name");
                if (namePropertyValue == null)
                {
                    continue;
                }

                //Nameプロパティ文字列の一部が"(COM1)～(COM999)"と一致するときリストに追加"
                string display = namePropertyValue.ToString();
                if (check.IsMatch(display))
                {        
                    // https://guminote.hatenablog.jp/entry/2015/08/13/193216
                    var ExtractPortNum = new System.Text.RegularExpressions.Regex(".*(COM[1-9][0-9]?[0-9]?).*");
                    string name = ExtractPortNum.Replace(display, "$1");

                    var ExtractName = new Regex(@"(.*?)\s*\(COM\d+\)");

                    if(name.Length == 4)
                    {
                        display = name + "   " + ExtractName.Match(display).Groups[1].Value;
                    }
                    else
                    {
                        display = name + " " + ExtractName.Match(display).Groups[1].Value;
                    }
                    src.Add(new ItemSet(name, display));
                    int_Max = Math.Max(int_Max, display.Length);
                }
            }

            //https://www.ampita.jp/2023/05/dropdownlist/
            cmbPortName.DropDownWidth = (int_Max + 1) * 14;

            // https://qiita.com/KnowledgeU/items/3388f4c461076b05f6e5
            cmbPortName.DataSource = null;
            cmbPortName.DataSource = src;
            cmbPortName.DisplayMember = "ItemDisp";
            cmbPortName.ValueMember = "ItemValue";

            if (cmbPortName.Items.Count > 0)
            {
                cmbPortName.SelectedIndex = 0;
            }
        }

        private void btnClearRcv_Click(object sender, EventArgs e)
        {
            rcvTextBox.Clear();
            rcvTextBoxScroll.Clear();
        }

        private void btnSaveRcv_Click(object sender, EventArgs e)
        {
            //https://www.ipentec.com/document/csharp-text-save-to-file
            Stream myStream;
            SaveFileDialog saveFileDialog1 = new SaveFileDialog();

            saveFileDialog1.Filter = "txt files (*.txt)|*.txt|CSV files (*.csv)|*.csv|All files (*.*)|*.*";
            saveFileDialog1.FilterIndex = 0;
            saveFileDialog1.RestoreDirectory = true;

            if (saveFileDialog1.ShowDialog() == DialogResult.OK)
            {
                try
                {
                    using (StreamWriter sw = new StreamWriter(saveFileDialog1.FileName, false, Encoding.ASCII))
                    {
                        sw.Write(rcvTextBoxScroll.Text);
                    }

                    AutoClosingMessageBox.Show("File saved successfully.", "Save File", 1000);
                }
                catch (Exception ex)
                {
                    MessageBox.Show($"Failed to save file: {ex.Message}", "Save Error", MessageBoxButtons.OK, MessageBoxIcon.Error);
                }
            }
        }

        private void check_timestamp_CheckedChanged(object sender, EventArgs e)
        {
            //https://learn.microsoft.com/ja-jp/dotnet/api/system.windows.forms.control.enabled?view=windowsdesktop-7.0
            cmbTimestamp.Enabled=check_timestamp.Checked;
        }

        private void connectSerial(bool message=false)
        {
            serialPort1.PortName = cmbPortName.SelectedValue.ToString();

            serialPort1.BaudRate = Convert.ToInt32(cmbBaudRate.SelectedItem);

            //https://learn.microsoft.com/ja-jp/dotnet/csharp/programming-guide/types/how-to-convert-a-string-to-a-number
            serialPort1.DataBits = Convert.ToInt32(cmbDataBits.SelectedItem);

            //https://learn.microsoft.com/ja-jp/dotnet/api/system.io.ports.parity?view=dotnet-plat-ext-7.0
            serialPort1.Parity = (Parity)Enum.Parse(typeof(Parity),cmbParity.SelectedItem.ToString(), true);
            serialPort1.StopBits = (StopBits)Enum.Parse(typeof(StopBits), cmbStopBits.SelectedItem.ToString(), true);
            serialPort1.Handshake = (Handshake)Enum.Parse(typeof(Handshake), cmbHandshake.SelectedItem.ToString(), true);

            serialPort1.DtrEnable = checkDtrEnable.Checked;
            serialPort1.RtsEnable = checkRtsEnable.Checked;

            try
            {
                serialPort1.Open();

                connectButton.Text = "Disconnect";
                setEnabled(false);
            }
            catch (Exception ex)
            {
                if (message) { 
                MessageBox.Show(ex.Message);
                }
            }
        }

        private void timerReconnect_Tick(object sender, EventArgs e)
        {
            if (serialPort1.IsOpen == false)
            {
                connectButton.Text = "Connect";
                setEnabled(true);
                if (reconnect.Checked)
                {
                    connectSerial();
                }
            }

        }
        private void initSettings() {
            //https://se-naruhodo.com/paramsettings/
            if (Properties.Settings.Default.setting_first)
            {
                Properties.Settings.Default.Upgrade();
                Properties.Settings.Default.setting_first = false;
            }

            //https://learn.microsoft.com/ja-jp/dotnet/api/system.io.ports.serialport?view=dotnet-plat-ext-7.0
            cmbBaudRate.SelectedIndex = Properties.Settings.Default.setting_BaudRate;
            cmbDataBits.SelectedIndex = Properties.Settings.Default.setting_DataBits;
            cmbParity.SelectedIndex = Properties.Settings.Default.setting_Parity;
            checkRtsEnable.Checked = Properties.Settings.Default.setting_RtsEnable;
            checkDtrEnable.Checked = Properties.Settings.Default.setting_DtrEnable;
            cmbStopBits.SelectedIndex = Properties.Settings.Default.setting_StopBits;
            cmbHandshake.SelectedIndex = Properties.Settings.Default.setting_Handshake;

            reconnect.Checked = Properties.Settings.Default.setting_reconnect;

            cmbCRLF.SelectedIndex = Properties.Settings.Default.setting_sendCRLF;
            checkEnter.Checked= Properties.Settings.Default.setting_Enter;

            checkWrap.Checked = Properties.Settings.Default.setting_Wrap;
            checkCRLF.Checked = Properties.Settings.Default.setting_CRLF;
            checkNULL.Checked = Properties.Settings.Default.setting_NULL;
            checkBIN.Checked =  Properties.Settings.Default.setting_binary;
            checkSendBIN.Checked = Properties.Settings.Default.setting_sendBinary;
            autoScroll.Checked = Properties.Settings.Default.setting_autoScroll;
            check_timestamp.Checked = Properties.Settings.Default.setting_timestamp;
            cmbTimestamp.SelectedIndex = Properties.Settings.Default.setting_timestamp_string;
            cmbTimestamp.Enabled = check_timestamp.Checked;

            textNote.Text = Properties.Settings.Default.setting_note;

            serialPort1.RtsEnable = checkRtsEnable.Checked;
            serialPort1.DtrEnable= checkDtrEnable.Checked;

            sndTextBox.AcceptsReturn = !checkEnter.Checked;

            //http://my-notepad.jugem.jp/?eid=8
            rcvTextBox.LanguageOption = RichTextBoxLanguageOptions.UIFonts;

            rcvTextBox.WordWrap = checkWrap.Checked;
            rcvTextBoxScroll.WordWrap = checkWrap.Checked;
            if (autoScroll.Checked)
            {
                rcvTextBoxScroll.BringToFront();
            }
            else
            {
                rcvTextBox.BringToFront();
            }

            checkCRLF.Enabled = !checkBIN.Checked;
            checkNULL.Enabled = !checkBIN.Checked;

            cmbCRLF.Enabled = !checkSendBIN.Checked;
        }

        private void checkDtrEnable_CheckedChanged(object sender, EventArgs e)
        {
            if (checkDtrEnable.Checked)
            {

                serialPort1.DtrEnable = true;
            }
            else
            {
                serialPort1.DtrEnable = false;
            }
        }

        private void checkRtsEnable_CheckedChanged(object sender, EventArgs e)
        {
            if (checkRtsEnable.Checked)
            {

                serialPort1.RtsEnable = true;
            }
            else
            {
                serialPort1.RtsEnable = false;
            }
        }

        private void setEnabled(bool enable)
        {
            reload_COM.Enabled = enable;
            cmbPortName.Enabled = enable;
            cmbBaudRate.Enabled = enable;
            cmbDataBits.Enabled = enable;
            cmbParity.Enabled = enable;
            cmbStopBits.Enabled = enable;
            cmbHandshake.Enabled = enable;

            //https://robinegg.hatenadiary.jp/entry/20090930/p1
            //https://nuneno.cocolog-nifty.com/blog/2014/03/arduino-f24c.html
            checkDtrEnable.Enabled = true;
            checkRtsEnable.Enabled = true;
        }

        private void checkEnter_CheckedChanged(object sender, EventArgs e)
        {
            sndTextBox.AcceptsReturn = !checkEnter.Checked;
        }

        private void autoScroll_CheckedChanged(object sender, EventArgs e)
        {
            if (autoScroll.Checked)
            {
                //https://atmarkit.itmedia.co.jp/ait/articles/0505/13/news116.html
                rcvTextBoxScroll.BringToFront();
            }
            else
            {
                //https://jehupc.exblog.jp/9247597/
                rcvTextBox.AppendText(data_queue.ToString());
                data_queue.Clear();
                rcvTextBox.BringToFront();
                rcvTextBox.ScrollToCaret();
                sndTextBox.Focus();
            }
        }

        private void btnCopy_Click(object sender, EventArgs e)
        {
            if(string.IsNullOrEmpty(rcvTextBoxScroll.Text) == false) { 
                Clipboard.SetText(rcvTextBoxScroll.Text);
                AutoClosingMessageBox.Show("Copied", "Clip Board", 1000);
            }
            else
            {
                //https://dobon.net/vb/dotnet/form/msgbox.html
                MessageBox.Show("empty", "Clip Board", MessageBoxButtons.OK, MessageBoxIcon.Exclamation);
            }
        }

        //https://koga2020.hatenablog.com/entry/54275972
        //https://stackoverflow.com/questions/14522540/close-a-messagebox-after-several-seconds
        public class AutoClosingMessageBox
        {
            int text_max_length = 200; //最大の文字列長の指定
            System.Threading.Timer _timeoutTimer;
            string _caption;
            AutoClosingMessageBox(string text, string caption, int timeout)
            {
                if (text.Length > text_max_length) //内容の長さを制限する追加部分
                {
                    text = text.Substring(0, text_max_length);
                }
                //text = ( text.length > text_max_length ) ? text.Substring( 0, text_max_length ) : text; //三項目演算での記述方法　? の左側が true の時 : の左側が代入されます。 falseの時は右側
                _caption = caption;
                _timeoutTimer = new System.Threading.Timer(OnTimerElapsed, null, timeout, System.Threading.Timeout.Infinite);
                MessageBox.Show(text, caption);
            }
            public static void Show(string text, string caption, int timeout)
            {
                new AutoClosingMessageBox(text, caption, timeout);
            }
            void OnTimerElapsed(object state)
            {
                IntPtr mbWnd = FindWindow(null, _caption);
                if (mbWnd != IntPtr.Zero)
                {
                    SendMessage(mbWnd, WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
                }
                _timeoutTimer.Dispose();
            }
            const int WM_CLOSE = 0x0010;
            [System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true)]
            static extern IntPtr FindWindow(string lpClassName, string lpWindowName);
            [System.Runtime.InteropServices.DllImport("user32.dll", CharSet = System.Runtime.InteropServices.CharSet.Auto)]
            static extern IntPtr SendMessage(IntPtr hWnd, UInt32 Msg, IntPtr wParam, IntPtr lParam);
        }

        private void checkWrap_CheckedChanged(object sender, EventArgs e)
        {
            if (rcvTextBoxScroll.TextLength > 4096)
            {
                rcvTextBox.Clear();
                rcvTextBoxScroll.Clear();
            }
            rcvTextBox.WordWrap = checkWrap.Checked;
            rcvTextBoxScroll.WordWrap = checkWrap.Checked;
        }

        bool resizing=false;
        StringBuilder resizing_buf = new System.Text.StringBuilder();
        private void Form1_ResizeBegin(object sender, EventArgs e)
        {
            resizing =true;
            if (rcvTextBoxScroll.TextLength > 4096)
            {
                rcvTextBox.Clear();
                rcvTextBoxScroll.Clear();
            }
        }
        private void Form1_ResizeEnd(object sender, EventArgs e)
        {
            resizing = false;
            rcvTextBox.AppendText(resizing_buf.ToString());
            rcvTextBoxScroll.AppendText(resizing_buf.ToString());
            resizing_buf.Clear();
        }

        private void checkBIN_CheckedChanged(object sender, EventArgs e)
        {
            checkCRLF.Enabled = !checkBIN.Checked;
            checkNULL.Enabled = !checkBIN.Checked;
        }

        private void checkSendBIN_CheckedChanged(object sender, EventArgs e)
        {
            cmbCRLF.Enabled = !checkSendBIN.Checked;
        }
    }

    //https://ehbtj.com/info/think-about-improving-performance-of-serial-monitor/
    public static class FastDateTime
    {
        public static TimeSpan LocalUtcOffset { get; private set; }

        public static DateTime Now
        {
            get { return DateTime.UtcNow + LocalUtcOffset; }
        }

        static FastDateTime()
        {
            LocalUtcOffset = TimeZone.CurrentTimeZone.GetUtcOffset(DateTime.Now);
        }
    }
}
