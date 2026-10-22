// A small TN5250 display station for capturing screens from a real IBM i
// (tools/golden/screens.ps1). Written from RFC 1205 (5250 Telnet), RFC 2877
// (5250 Telnet enhancements: the NEW-ENVIRON variables KBDTYPE, CODEPAGE
// and CHARSET that pick the device's code page) and the public 5250 data
// stream descriptions, like as4tainr's own
// station (core/src/web/display.rs). C# 5, so Windows PowerShell's
// Add-Type compiles it without anything to install.
//
// It types into the input fields of the screen the host shows, presses AID
// keys, waits for the host to answer, and returns the screen as text. Text
// in non-display fields (a password) is never part of a screen it returns.

using System;
using System.Collections.Generic;
using System.IO;
using System.Net.Sockets;
using System.Text;
using System.Threading;

public class Tn5250Field
{
    public int Start;   // linear position of the first data position
    public int Len;
    public byte Ffw1, Ffw2;
    public bool HasFfw;
    public byte Attr;
    public bool Mdt;

    public bool IsInput { get { return HasFfw && (Ffw1 & 0x20) == 0; } }
    public bool Monocase { get { return HasFfw && (Ffw2 & 0x20) != 0; } }
    public bool NonDisplay { get { return (Attr & 0x07) == 0x07; } }
}

public class Tn5250Capture
{
    public const int Rows = 24, Cols = 80, Size = Rows * Cols;
    const byte IAC = 255, DONT = 254, DO = 253, WONT = 252, WILL = 251, SB = 250, SE = 240, EOR = 239;
    const byte OPT_BINARY = 0, OPT_TTYPE = 24, OPT_EOR = 25, OPT_NEW_ENVIRON = 39;

    TcpClient client;
    NetworkStream stream;
    byte[] buf = new byte[Size];
    List<Tn5250Field> fields = new List<Tn5250Field>();
    int cursor, addr;
    bool locked = true, readPending;
    string error;
    // Telnet parsing state.
    List<byte> record = new List<byte>();
    List<byte> sub = new List<byte>();
    int tstate; // 0 data, 1 IAC, 2 command, 3 sub, 4 sub IAC
    byte tcmd;
    public string TerminalType = "IBM-3179-2";
    public StringBuilder Log = new StringBuilder();
    // EBCDIC CCSID 37 to Unicode (the code page's published mapping), so
    // nothing depends on the encodings a .NET installation has.
    static readonly char[] ToUni = BuildToUni();
    static readonly Dictionary<char, byte> FromUni = BuildFromUni();

    static char[] BuildToUni()
    {
        int[] t = {
        0x0000, 0x0001, 0x0002, 0x0003, 0x009C, 0x0009, 0x0086, 0x007F, 0x0097, 0x008D, 0x008E, 0x000B,
        0x000C, 0x000D, 0x000E, 0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x009D, 0x0085, 0x0008, 0x0087,
        0x0018, 0x0019, 0x0092, 0x008F, 0x001C, 0x001D, 0x001E, 0x001F, 0x0080, 0x0081, 0x0082, 0x0083,
        0x0084, 0x000A, 0x0017, 0x001B, 0x0088, 0x0089, 0x008A, 0x008B, 0x008C, 0x0005, 0x0006, 0x0007,
        0x0090, 0x0091, 0x0016, 0x0093, 0x0094, 0x0095, 0x0096, 0x0004, 0x0098, 0x0099, 0x009A, 0x009B,
        0x0014, 0x0015, 0x009E, 0x001A, 0x0020, 0x00A0, 0x00E2, 0x00E4, 0x00E0, 0x00E1, 0x00E3, 0x00E5,
        0x00E7, 0x00F1, 0x00A2, 0x002E, 0x003C, 0x0028, 0x002B, 0x007C, 0x0026, 0x00E9, 0x00EA, 0x00EB,
        0x00E8, 0x00ED, 0x00EE, 0x00EF, 0x00EC, 0x00DF, 0x0021, 0x0024, 0x002A, 0x0029, 0x003B, 0x00AC,
        0x002D, 0x002F, 0x00C2, 0x00C4, 0x00C0, 0x00C1, 0x00C3, 0x00C5, 0x00C7, 0x00D1, 0x00A6, 0x002C,
        0x0025, 0x005F, 0x003E, 0x003F, 0x00F8, 0x00C9, 0x00CA, 0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF,
        0x00CC, 0x0060, 0x003A, 0x0023, 0x0040, 0x0027, 0x003D, 0x0022, 0x00D8, 0x0061, 0x0062, 0x0063,
        0x0064, 0x0065, 0x0066, 0x0067, 0x0068, 0x0069, 0x00AB, 0x00BB, 0x00F0, 0x00FD, 0x00FE, 0x00B1,
        0x00B0, 0x006A, 0x006B, 0x006C, 0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072, 0x00AA, 0x00BA,
        0x00E6, 0x00B8, 0x00C6, 0x00A4, 0x00B5, 0x007E, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077, 0x0078,
        0x0079, 0x007A, 0x00A1, 0x00BF, 0x00D0, 0x00DD, 0x00DE, 0x00AE, 0x005E, 0x00A3, 0x00A5, 0x00B7,
        0x00A9, 0x00A7, 0x00B6, 0x00BC, 0x00BD, 0x00BE, 0x005B, 0x005D, 0x00AF, 0x00A8, 0x00B4, 0x00D7,
        0x007B, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047, 0x0048, 0x0049, 0x00AD, 0x00F4,
        0x00F6, 0x00F2, 0x00F3, 0x00F5, 0x007D, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F, 0x0050,
        0x0051, 0x0052, 0x00B9, 0x00FB, 0x00FC, 0x00F9, 0x00FA, 0x00FF, 0x005C, 0x00F7, 0x0053, 0x0054,
        0x0055, 0x0056, 0x0057, 0x0058, 0x0059, 0x005A, 0x00B2, 0x00D4, 0x00D6, 0x00D2, 0x00D3, 0x00D5,
        0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x00B3, 0x00DB,
        0x00DC, 0x00D9, 0x00DA, 0x009F,
        };
        char[] c = new char[256];
        for (int k = 0; k < 256; k++) c[k] = (char)t[k];
        return c;
    }

    static Dictionary<char, byte> BuildFromUni()
    {
        Dictionary<char, byte> d = new Dictionary<char, byte>();
        for (int k = 0; k < 256; k++) if (!d.ContainsKey(ToUni[k])) d[ToUni[k]] = (byte)k;
        return d;
    }

    static byte ToEbcdic(char c)
    {
        byte b;
        return FromUni.TryGetValue(c, out b) ? b : (byte)0x6F; // '?'
    }

    public void Connect(string host, int port)
    {
        client = new TcpClient(host, port);
        stream = client.GetStream();
    }

    public void Close()
    {
        try { client.Close(); } catch { }
    }

    void Send(byte[] b)
    {
        stream.Write(b, 0, b.Length);
    }

    // Read what the host sends until it has been quiet for quietMs and the
    // keyboard is unlocked, or until maxMs. Returns the number of records.
    public int Pump(int quietMs, int maxMs)
    {
        int records = 0;
        DateTime start = DateTime.Now, last = DateTime.Now;
        byte[] tmp = new byte[8192];
        while (true)
        {
            if (stream.DataAvailable)
            {
                int n = stream.Read(tmp, 0, tmp.Length);
                if (n <= 0) break;
                records += Feed(tmp, n);
                last = DateTime.Now;
                continue;
            }
            double quiet = (DateTime.Now - last).TotalMilliseconds;
            double total = (DateTime.Now - start).TotalMilliseconds;
            if (total > maxMs) break;
            if (quiet > quietMs && !locked && records > 0) break;
            Thread.Sleep(20);
        }
        return records;
    }

    int Feed(byte[] data, int n)
    {
        int records = 0;
        for (int k = 0; k < n; k++)
        {
            byte b = data[k];
            switch (tstate)
            {
                case 0:
                    if (b == IAC) tstate = 1; else record.Add(b);
                    break;
                case 1:
                    if (b == IAC) { record.Add(IAC); tstate = 0; }
                    else if (b == EOR) { OnRecord(record.ToArray()); records++; record.Clear(); tstate = 0; }
                    else if (b == SB) { sub.Clear(); tstate = 3; }
                    else if (b == DO || b == DONT || b == WILL || b == WONT) { tcmd = b; tstate = 2; }
                    else tstate = 0;
                    break;
                case 2:
                    OnOption(tcmd, b);
                    tstate = 0;
                    break;
                case 3:
                    if (b == IAC) tstate = 4; else sub.Add(b);
                    break;
                case 4:
                    if (b == SE) { OnSub(sub.ToArray()); tstate = 0; }
                    else { sub.Add(b); tstate = 3; }
                    break;
            }
        }
        return records;
    }

    void OnOption(byte cmd, byte opt)
    {
        bool known = opt == OPT_BINARY || opt == OPT_TTYPE || opt == OPT_EOR || opt == OPT_NEW_ENVIRON;
        if (cmd == DO) Send(new byte[] { IAC, known ? WILL : WONT, opt });
        else if (cmd == WILL) Send(new byte[] { IAC, (opt == OPT_BINARY || opt == OPT_EOR) ? DO : DONT, opt });
        Log.AppendLine("telnet " + cmd + " " + opt);
    }

    void OnSub(byte[] s)
    {
        // TERMINAL-TYPE SEND: answer IS IBM-3179-2.
        if (s.Length >= 2 && s[0] == OPT_TTYPE && s[1] == 1)
        {
            List<byte> r = new List<byte> { IAC, SB, OPT_TTYPE, 0 };
            r.AddRange(Encoding.ASCII.GetBytes(TerminalType));
            r.Add(IAC); r.Add(SE);
            Send(r.ToArray());
        }
        // NEW-ENVIRON SEND (RFC 4777): the device is a US keyboard with code
        // page 37 (character set 697), the table this client types and reads
        // with. Without it the server takes the system's keyboard type, and a
        // password with characters that differ between code pages (PUB400 is
        // CCSID 273) is not the password typed. No sign-on values are sent.
        else if (s.Length >= 2 && s[0] == OPT_NEW_ENVIRON && s[1] == 1)
        {
            List<byte> r = new List<byte> { IAC, SB, OPT_NEW_ENVIRON, 0 };
            string[,] vars = { { "KBDTYPE", "USB" }, { "CODEPAGE", "37" }, { "CHARSET", "697" } };
            for (int k = 0; k < vars.GetLength(0); k++)
            {
                r.Add(3); r.AddRange(Encoding.ASCII.GetBytes(vars[k, 0])); // USERVAR
                r.Add(1); r.AddRange(Encoding.ASCII.GetBytes(vars[k, 1])); // VALUE
            }
            r.Add(IAC); r.Add(SE);
            Send(r.ToArray());
            Log.AppendLine("sent NEW-ENVIRON KBDTYPE USB, CODEPAGE 37, CHARSET 697");
        }
    }

    void SendRecord(byte opcode, byte[] data)
    {
        int len = data.Length + 10;
        List<byte> rec = new List<byte> {
            (byte)(len >> 8), (byte)len, 0x12, 0xA0, 0x00, 0x00, 0x04, 0x00, 0x00, opcode };
        rec.AddRange(data);
        List<byte> o = new List<byte>();
        foreach (byte b in rec) { o.Add(b); if (b == IAC) o.Add(IAC); }
        o.Add(IAC); o.Add(EOR);
        Send(o.ToArray());
    }

    void OnRecord(byte[] rec)
    {
        if (rec.Length < 10 || rec[2] != 0x12 || rec[3] != 0xA0) { Log.AppendLine("not a GDS record"); return; }
        int start = 6 + rec[6];
        byte opcode = rec[9];
        Log.AppendLine("record opcode " + opcode.ToString("X2") + " length " + rec.Length);
        if (start >= rec.Length) return;
        byte[] data = new byte[rec.Length - start];
        Array.Copy(rec, start, data, 0, data.Length);
        string skipped = Apply(data);
        if (skipped != null) Log.AppendLine("skipped: " + skipped);
    }

    static int Pos(int r, int c)
    {
        if (r >= 1 && r <= Rows && c >= 1 && c <= Cols) return (r - 1) * Cols + c - 1;
        throw new InvalidDataException("address " + r + "," + c + " is off the screen");
    }

    void Put(byte b) { buf[addr] = b; addr = (addr + 1) % Size; }

    string Apply(byte[] d)
    {
        int i = 0;
        while (i < d.Length)
        {
            if (d[i] != 0x04) return "expected a command at " + i + ", found " + d[i].ToString("X2");
            if (i + 1 >= d.Length) break;
            byte c = d[i + 1];
            i += 2;
            switch (c)
            {
                case 0x40: case 0x20: // Clear Unit (Alternate)
                    if (c == 0x20) i++;
                    Array.Clear(buf, 0, Size); fields.Clear(); cursor = 0; addr = 0; error = null; locked = true; readPending = false;
                    break;
                case 0x50: fields.Clear(); locked = true; break; // Clear Format Table
                case 0x11: // Write To Display
                    {
                        byte cc1 = i < d.Length ? d[i] : (byte)0, cc2 = i + 1 < d.Length ? d[i + 1] : (byte)0;
                        i += 2;
                        locked = true;
                        int top = cc1 & 0xE0;
                        if (top == 0x40 || top == 0x60 || top == 0xA0 || top == 0xC0 || top == 0xE0)
                            foreach (Tn5250Field f in fields) f.Mdt = false;
                        string e = Orders(d, ref i);
                        if (e != null) return e;
                        if ((cc2 & 0x08) != 0) locked = false;
                        break;
                    }
                case 0x21: // Write Error Code
                    {
                        int s = i;
                        while (i < d.Length && d[i] != 0x04) i++;
                        StringBuilder t = new StringBuilder();
                        for (int k = s; k < i; k++) if (d[k] >= 0x40) t.Append(ToUni[d[k]]);
                        error = t.ToString().Trim();
                        break;
                    }
                case 0x42: case 0x52: case 0x82: // Read Input / MDT Fields
                    i += 2; readPending = true; locked = false;
                    break;
                case 0xF3: // Write Structured Field: answer the 5250 Query
                    {
                        int len = i + 1 < d.Length ? (d[i] << 8) | d[i + 1] : 2;
                        bool query = i + 3 < d.Length && d[i + 2] == 0xD9 && d[i + 3] == 0x70;
                        i += Math.Max(len, 2);
                        if (query) QueryReply();
                        break;
                    }
                default:
                    return "command " + c.ToString("X2") + " is not supported";
            }
        }
        return null;
    }

    // The 5250 Query reply (an inbound structured field, AID 0x88), with the
    // values tn5250 sends: a display station 3179 model 2.
    void QueryReply()
    {
        byte[] t = new byte[61];
        t[2] = 0x88; t[3] = 0x00; t[4] = 0x3A; t[5] = 0xD9; t[6] = 0x70; t[7] = 0x80;
        t[8] = 0x06; t[9] = 0x00; t[10] = 0x01; t[11] = 0x01; t[12] = 0x00;
        t[29] = 0x01;
        string dev = "3179 02";
        for (int k = 0; k < 7; k++) t[30 + k] = ToEbcdic(dev[k]);
        t[37] = 0x02;
        t[41] = 0x61; t[42] = 0x50;
        t[44] = 0xFF; t[45] = 0xFF;
        t[49] = 0x23; t[50] = 0x31;
        SendRecord(0x00, t);
        Log.AppendLine("sent query reply");
    }

    string Orders(byte[] d, ref int i)
    {
        while (i < d.Length && d[i] != 0x04)
        {
            byte b = d[i];
            switch (b)
            {
                case 0x11: addr = Pos(d[i + 1], d[i + 2]); i += 3; break; // SBA
                case 0x13: case 0x14: cursor = Pos(d[i + 1], d[i + 2]); i += 3; break; // IC, MC
                case 0x02: // RA
                    {
                        int to = Pos(d[i + 1], d[i + 2]); byte ch = d[i + 3]; i += 4;
                        while (true) { int at = addr; Put(ch); if (at == to || addr == 0) break; }
                        break;
                    }
                case 0x03: // EA
                    {
                        int to = Pos(d[i + 1], d[i + 2]); int n = d[i + 3]; i += 3 + Math.Max(n, 1);
                        while (true) { int at = addr; Put(0); if (at == to || addr == 0) break; }
                        break;
                    }
                case 0x01: { int n = d[i + 1]; i += 1 + Math.Max(n, 1); break; } // SOH
                case 0x12: i += 3; break; // WEA
                case 0x10: case 0x15: // TD, WDSF
                    {
                        int n = (d[i + 1] << 8) | d[i + 2];
                        if (b == 0x10) { for (int k = 0; k < n; k++) Put(d[i + 3 + k]); i += 3 + n; }
                        else i += 1 + Math.Max(n, 2);
                        break;
                    }
                case 0x1D: // SF
                    {
                        i++;
                        Tn5250Field f = new Tn5250Field();
                        if ((d[i] & 0xC0) == 0x40)
                        {
                            f.HasFfw = true; f.Ffw1 = d[i]; f.Ffw2 = d[i + 1]; i += 2;
                            while ((d[i] & 0xC0) == 0x80) i += 2; // field control words
                        }
                        f.Attr = d[i];
                        int len = (d[i + 1] << 8) | d[i + 2];
                        i += 3;
                        Put(f.Attr);
                        f.Start = addr;
                        f.Len = Math.Min(len, Size - f.Start);
                        f.Mdt = f.HasFfw && (f.Ffw1 & 0x08) != 0;
                        fields.RemoveAll(x => x.Start == f.Start);
                        fields.Add(f);
                        fields.Sort((x, y) => x.Start.CompareTo(y.Start));
                        break;
                    }
                default:
                    if (b == 0x00 || b == 0x1C || b == 0x0E || b == 0x0F || b >= 0x20) { Put(b); i++; }
                    else return "order " + b.ToString("X2") + " is not supported";
                    break;
            }
        }
        return null;
    }

    Tn5250Field FieldAt(int p)
    {
        foreach (Tn5250Field f in fields) if (f.IsInput && p >= f.Start && p < f.Start + f.Len) return f;
        return null;
    }

    Tn5250Field NextInput(int p)
    {
        foreach (Tn5250Field f in fields) if (f.IsInput && f.Start > p) return f;
        foreach (Tn5250Field f in fields) if (f.IsInput) return f;
        return null;
    }

    // Type text at the cursor, as an operator would (into input fields only).
    public void Type(string text)
    {
        Tn5250Field f = FieldAt(cursor);
        if (f == null) { f = NextInput(cursor); if (f == null) return; cursor = f.Start; }
        foreach (char ch in text)
        {
            if (cursor >= f.Start + f.Len) { f = NextInput(cursor - 1); if (f == null) return; cursor = f.Start; }
            char c = f.Monocase ? char.ToUpperInvariant(ch) : ch;
            byte e = ToEbcdic(c);
            buf[cursor] = e >= 0x40 ? e : (byte)0x40;
            f.Mdt = true;
            cursor++;
        }
    }

    // Field Exit: the rest of the field is cleared, then the next field.
    public void FieldExit()
    {
        Tn5250Field f = FieldAt(cursor);
        if (f != null)
        {
            for (int p = cursor; p < f.Start + f.Len; p++) buf[p] = 0;
            f.Mdt = true;
        }
        Tab();
    }

    // Erase to the end of the field, the cursor staying where it is.
    public void EraseEof()
    {
        Tn5250Field f = FieldAt(cursor);
        if (f == null) return;
        for (int p = cursor; p < f.Start + f.Len; p++) buf[p] = 0;
        f.Mdt = true;
    }

    public void Tab()
    {
        Tn5250Field f = NextInput(cursor);
        if (f != null) cursor = f.Start;
    }

    public static byte AidByte(string name)
    {
        string n = name.ToUpperInvariant();
        switch (n)
        {
            case "ENTER": return 0xF1;
            case "HELP": return 0xF3;
            case "PAGEUP": case "ROLLDOWN": return 0xF4;
            case "PAGEDOWN": case "ROLLUP": return 0xF5;
            case "CLEAR": return 0xBD;
        }
        if (n.StartsWith("F"))
        {
            int k;
            if (int.TryParse(n.Substring(1), out k))
            {
                if (k >= 1 && k <= 12) return (byte)(0x30 + k);
                if (k >= 13 && k <= 24) return (byte)(0xB0 + k - 12);
            }
        }
        return 0;
    }

    // Press an AID key: the cursor, the AID and the changed input fields.
    public bool Aid(string name)
    {
        byte aid = AidByte(name);
        if (aid == 0) throw new ArgumentException("unknown key " + name);
        if (locked || !readPending) { Log.AppendLine("keyboard locked: " + name + " not sent"); return false; }
        List<byte> o = new List<byte> { (byte)(cursor / Cols + 1), (byte)(cursor % Cols + 1), aid };
        foreach (Tn5250Field f in fields)
        {
            if (!f.Mdt || !f.IsInput) continue;
            int used = 0;
            for (int p = 0; p < f.Len; p++) if (buf[f.Start + p] != 0) used = p + 1;
            o.Add(0x11); o.Add((byte)(f.Start / Cols + 1)); o.Add((byte)(f.Start % Cols + 1));
            for (int p = 0; p < used; p++) { byte b = buf[f.Start + p]; o.Add(b == 0 ? (byte)0x40 : b); }
        }
        locked = true; readPending = false;
        SendRecord(0x03, o.ToArray());
        return true;
    }

    // The screen as text: 24 lines (attribute bytes and nulls as blanks,
    // non-display fields blank), then the cursor, the input fields and the
    // error line.
    public string Screen()
    {
        bool[] hidden = new bool[Size];
        foreach (Tn5250Field f in fields)
            if (f.NonDisplay) for (int p = f.Start; p < f.Start + f.Len && p < Size; p++) hidden[p] = true;
        StringBuilder s = new StringBuilder();
        for (int r = 0; r < Rows; r++)
        {
            StringBuilder line = new StringBuilder();
            for (int c = 0; c < Cols; c++)
            {
                int p = r * Cols + c;
                byte b = buf[p];
                if (hidden[p] || b < 0x40) line.Append(' ');
                else line.Append(ToUni[b]);
            }
            s.AppendLine(line.ToString().TrimEnd());
        }
        s.AppendLine("cursor=" + (cursor / Cols + 1) + "," + (cursor % Cols + 1) + (locked ? " locked" : ""));
        foreach (Tn5250Field f in fields)
            s.AppendLine("field " + (f.Start / Cols + 1) + "," + (f.Start % Cols + 1) + " len=" + f.Len +
                " attr=" + f.Attr.ToString("X2") + (f.HasFfw ? " ffw=" + f.Ffw1.ToString("X2") + f.Ffw2.ToString("X2") : "") +
                (f.IsInput ? " input" : ""));
        if (error != null) s.AppendLine("error=" + error);
        return s.ToString();
    }

    // Run one step: text with {KEY} tokens ({TAB}, {FIELDEXIT}, {ERASEEOF}, {ENTER},
    // {F1}-{F24}, {PAGEUP}, {PAGEDOWN}, {WAIT n}); {USER} and {PASSWORD} type
    // the values given. After an AID key it waits for the host's answer.
    public bool Step(string actions, string user, string password, int quietMs)
    {
        int i = 0;
        while (i < actions.Length)
        {
            if (actions[i] == '{')
            {
                int j = actions.IndexOf('}', i);
                string tok = actions.Substring(i + 1, j - i - 1).Trim();
                i = j + 1;
                string up = tok.ToUpperInvariant();
                if (up == "TAB") Tab();
                else if (up == "FIELDEXIT") FieldExit();
                else if (up == "ERASEEOF") EraseEof();
                else if (up == "USER") Type(user);
                else if (up == "PASSWORD") Type(password);
                else if (up.StartsWith("WAIT")) Thread.Sleep(1000 * int.Parse(up.Substring(4).Trim()));
                else if (AidByte(up) != 0) { if (!Aid(up)) return false; Pump(quietMs, 60000); }
                else throw new ArgumentException("unknown token {" + tok + "}");
            }
            else
            {
                int j = actions.IndexOf('{', i);
                if (j < 0) j = actions.Length;
                Type(actions.Substring(i, j - i));
                i = j;
            }
        }
        return true;
    }

    // An input field whose text is not shown: a password is asked for.
    public bool HasNonDisplayInput()
    {
        foreach (Tn5250Field f in fields) if (f.IsInput && f.NonDisplay) return true;
        return false;
    }

    // Run a list of steps ("name|actions" lines, '#' comments) and write each
    // screen to outDir/<name>.txt. The first screen (before any step) is
    // written as 00_initial.txt.
    public static int RunList(string host, int port, string listPath, string outDir,
                              string user, string password, int quietMs)
    {
        Directory.CreateDirectory(outDir);
        Tn5250Capture t = new Tn5250Capture();
        t.Connect(host, port);
        int done = 0;
        try
        {
            t.Pump(quietMs, 30000);
            File.WriteAllText(Path.Combine(outDir, "00_initial.txt"), t.Screen());
            foreach (string raw in File.ReadAllLines(listPath))
            {
                string line = raw.Trim();
                if (line.Length == 0 || line.StartsWith("#")) continue;
                int bar = line.IndexOf('|');
                string name = line.Substring(0, bar).Trim();
                string actions = line.Substring(bar + 1);
                Console.WriteLine(name + " ...");
                bool signOn = actions.IndexOf("{PASSWORD}", StringComparison.OrdinalIgnoreCase) >= 0;
                // Never type anything into a sign-on screen except in the
                // sign-on step: a step typed into the user field is another
                // sign-on attempt, and IBM i disables the device or the
                // profile after QMAXSIGN attempts.
                if (!signOn && t.HasNonDisplayInput())
                    throw new InvalidOperationException("stopped before " + name + ": the screen asks for a password (not signed on)");
                if (!t.Step(actions, user, password, quietMs))
                {
                    File.WriteAllText(Path.Combine(outDir, name + ".txt"), t.Screen());
                    throw new InvalidOperationException("stopped at " + name + ": the keyboard was locked, a key was not sent");
                }
                File.WriteAllText(Path.Combine(outDir, name + ".txt"), t.Screen());
                done++;
                if (signOn && t.HasNonDisplayInput())
                    throw new InvalidOperationException("stopped after " + name + ": the sign-on was not accepted (see " + name + ".txt); nothing else is typed, so no more sign-on attempts are made");
            }
        }
        finally
        {
            File.WriteAllText(Path.Combine(outDir, "session.log"), t.Log.ToString());
            t.Close();
        }
        return done;
    }
}
