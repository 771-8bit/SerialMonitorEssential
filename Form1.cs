using System;
using System.Collections;
using System.Collections.Generic;
using System.ComponentModel;
using System.Data;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.IO.Ports;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using System.Windows.Forms;
using static System.Net.Mime.MediaTypeNames;
using static System.Windows.Forms.VisualStyles.VisualStyleElement;
using static System.Windows.Forms.VisualStyles.VisualStyleElement.Button;
using static System.Windows.Forms.VisualStyles.VisualStyleElement.Window;

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
        private void Form1_Load(object sender, EventArgs e)
        {
            reload_COM_Click(sender, e);

            initSettings();

            AcceptButton = btnSend;

            setEnabled(true);
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
            if (string.IsNullOrEmpty(data))
            {
                return;
            }
            try
            {
                switch (cmbCRLF.SelectedIndex)
                {
                    case 0:
                        serialPort1.Write(data);
                        break;
                    case 1:
                        serialPort1.Write(data+"\n");
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
                sndTextBox.Clear();
                //https://atmarkit.itmedia.co.jp/ait/articles/1004/08/news094.html
                if (string.IsNullOrEmpty(data.Replace("\r", "").Replace("\n", ""))==false)
                {
                    bool update = false;
                    if (previous_send_data.Count == 0) {
                        update = true;
                    }
                    else
                    {
                        if (previous_send_data[previous_send_data.Count - 1] != data)  {
                            update = true;
                        }
                    }
                    if (update)
                    {
                        previous_send_data.Add(data);
                    }
                }
                previous_send_data_index = previous_send_data.Count;
            }
            catch (Exception ex)
            {
                MessageBox.Show(ex.Message);
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
        private void timer1_Tick(object sender, EventArgs e)
        {
            //https://atmarkit.itmedia.co.jp/ait/articles/0511/11/news117.html
            if (serialPort1.IsOpen == false)
            {
                return;
            }

            try
            {
                string rawdata = serialPort1.ReadExisting();
                /*
                for(int i = 0;i< rawdata.Length;i++)
                {
                    int code = rawdata[i];
                    Console.Write("{0:X}",code);
                    Console.Write(",");
                }*/
                if (string.IsNullOrEmpty(rawdata)==false)
                {
                    //Console.Write("\r\n");
                    StringBuilder new_text = new System.Text.StringBuilder();
                    String timestamp = String.Format("{0:HH:mm:ss.f}{1}", FastDateTime.Now, cmbTimestamp.SelectedItem.ToString());

                    if (last_is_newline && check_timestamp.Checked)
                    {
                        new_text.Append(timestamp);
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

                    string newline = "\r\n";
                    bool timestampChecked = check_timestamp.Checked;
                    if (timestampChecked)
                    {
                        newline = "\r\n" + timestamp;
                    }
                    //https://dobon.net/vb/dotnet/string/replace.html
                    bool CRLFchecked = checkCRLF.Checked;
                    if (CRLFchecked)
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
                        if(timestampChecked) { 
                            new_text.Remove(new_text.Length- (timestamp.Length), (timestamp.Length));
                        }
                    }
                    else
                    {
                        last_is_newline = false;
                    }

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

        private void reload_COM_Click(object sender, EventArgs e)
        {
            string[] PortList = SerialPort.GetPortNames();

            cmbPortName.Items.Clear();

            foreach (string PortName in PortList)
            {
                cmbPortName.Items.Add(PortName);
            }
            if (cmbPortName.Items.Count > 0)
            {
                cmbPortName.SelectedIndex = 0;
            }
        }

        //https://hironimo.com/prog/c-sharp/c-keypress/
        private void sndTextBox_KeyDown(object sender, KeyEventArgs e)
        {
            if (e.KeyCode == Keys.Up)
            {
                if (previous_send_data_index >= 1 && previous_send_data.Count > previous_send_data_index-1 && previous_send_data.Count != 0)
                {
                    previous_send_data_index--;
                    sndTextBox.Clear();
                    sndTextBox.AppendText(previous_send_data[previous_send_data_index]);
                }
            }
            if (e.KeyCode == Keys.Down)
            {
                if (previous_send_data_index >= -1 && previous_send_data.Count > previous_send_data_index+1)
                {
                    previous_send_data_index++;
                    sndTextBox.Clear();
                    sndTextBox.AppendText(previous_send_data[previous_send_data_index]);
                }else if (previous_send_data.Count == previous_send_data_index + 1) { 
                    previous_send_data_index++;
                    sndTextBox.Clear();
                }
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
                if ((myStream = saveFileDialog1.OpenFile()) != null)
                {
                    myStream.Close();
                    StreamWriter sw = new StreamWriter(saveFileDialog1.FileName, false, Encoding.ASCII);
                    sw.Write(rcvTextBoxScroll.Text);
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
            serialPort1.PortName = cmbPortName.SelectedItem.ToString();

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
            checkDtrEnable.Checked = true;
            checkRtsEnable.Checked = false;

            //https://learn.microsoft.com/ja-jp/dotnet/api/system.io.ports.serialport?view=dotnet-plat-ext-7.0
            cmbDataBits.SelectedIndex= 1;//8bit
            cmbParity.SelectedIndex = 0;//None
            cmbStopBits.SelectedIndex = 0;//one
            cmbHandshake.SelectedIndex = 0;//none

            cmbTimestamp.SelectedIndex = 0;
            cmbBaudRate.SelectedIndex = 5;//115200
            cmbCRLF.SelectedIndex = 0;//no CRLF

            sndTextBox.AcceptsReturn = !checkEnter.Checked;
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
            resizing_buf.Clear();
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
