#!/usr/bin/env python3
"""XMLSERVICE's own tests (test/php/*.phpt of IBM/xmlservice), run without
PHP: the XML each test sends (its <<<ENDPROC here-document, the library
name put in as its test_lib_replace does) goes through the same iPLUG
procedure, with the control flags the test sets, in stateless mode (*here;
the tests' default *sbmjob needs XMLSERVICE's IPC server job). The PHP
code is never run: this reads the test files as data.

A test passes when what comes back meets the test's own checks of the
text: XML that parses when the test parses it (simplexml_load_string),
none of the strings whose presence fails it (strpos(...)>0: die; when it
has none, XMLSERVICE's <error> elements, but for a DLTxxx command's: a
script deletes first what an earlier run made), all of those whose absence
fails it (strpos(...)<1: die). The values the tests then check through
XPath are not checked here. Result-set procedures (iPLUGR*) give their
rows joined (as4tainr sql --json), cut at the first null character, and
with *hack at the first </hack> (xmlservice_junk_away.php's
driverJunkAway). A test that builds its XML with code is left out, but
for bin2hex() replacements of literals, and so is one that calls more
than once, and so are those that read the QTEMP table test_90000 made
in its job. The SQL tests' tables and procedures are made first as
test_90000 makes them (in schema XMLSERVTST), and the SQL tests run with
XMLSERVTST in the library list (test_90000's CHGLIBL).

  tools/xmlservice_tests.py XMLSERVICE-CLONE [-v] [-k NAME...]

Env: AS4TAINR_BIN (default target/debug/as4tainr), AS4TAINR_DATABASE_URL.
"""
import json, os, re, subprocess, sys
import xml.etree.ElementTree as ET


def tests(clone):
    d = os.path.join(clone, 'test', 'php')
    for f in sorted(os.listdir(d)):
        if not f.endswith('.phpt'):
            continue
        t = open(os.path.join(d, f), encoding='latin-1').read()
        # One call per test: a test that sends its XML in parts over
        # calls on one connection (iPLUGRC*'s accumulation) is left out.
        if len(re.findall(r'db2_execute\(', t)) > 1:
            continue
        m = re.search(r'call \$procLib\.(iPLUG\w+)\(', t)
        x = re.search(r'<<<ENDPROC\n(.*?)\nENDPROC;', t, re.S)
        if not m or not x:
            continue
        xml = x.group(1).replace('xyzlibxmlservicexyz', 'XMLSERVICE')
        if '$' in xml:  # PHP variables inside the XML
            continue
        # QTEMP's table that test_90000 made in its own job.
        if 'animalq' in xml.lower():
            continue
        # Text the test replaces after the here-document: only its
        # bin2hex("...") replacements are done here; other tests that
        # build their XML with code are left out.
        after = t[x.end():t.find('}', x.end())]
        if 'str_replace' in after:
            xml = replaced(xml, after)
            if xml is None:
                continue
        ctl = '*here'
        for op, val in re.findall(r'\$ctl\s*(\.?=)\s*"([^"]*)"', t):
            ctl = val if op == '=' else ctl + val
        # The test's own checks of the text that came back:
        # strpos($clobOut,"x")>0 fails, strpos($clobOut,"x")<1 fails.
        lit = r'strpos\(\$clobOut,\s*["\']([^"\'$]+)["\']\)\s*'
        checks = {
            'parse': 'simplexml_load_string' in t,
            'absent': re.findall(lit + r'>\s*0\)\s*die', t),
            'present': re.findall(lit + r'<\s*1\)\s*die', t),
        }
        yield f, m.group(1).upper(), xml, ctl.strip(), checks


def replaced(xml, code):
    """str_replace($was, $now, $clob) where $now's items are bin2hex() of
    literals (the library placeholder put in); None for other code."""
    hexes = {}
    for line in code.strip().splitlines():
        line = line.strip()
        m = re.fullmatch(r'\$(\w+)\s*=\s*bin2hex\((?:test_lib_replace\()?"([^"]*)"\)?\);', line)
        if m:
            text = m.group(2).replace('xyzlibxmlservicexyz', 'XMLSERVICE')
            hexes[m.group(1)] = text.encode('latin-1').hex()
            continue
        if line.startswith(('$was', '$now', 'return str_replace')):
            continue
        return None
    was = re.search(r'\$was\s*=\s*array\(([^)]*)\);', code)
    now = re.search(r'\$now\s*=\s*array\(([^)]*)\);', code)
    if not was or not now:
        return None
    olds = re.findall(r'"([^"]*)"', was.group(1))
    news = [hexes.get(v) for v in re.findall(r'\$(\w+)', now.group(1))]
    if len(olds) != len(news) or None in news:
        return None
    for o, n in zip(olds, news):
        xml = xml.replace(o, n)
    return xml


def sql_setup(clone, binary):
    """The SQL tests' tables and procedures (test_90000's actions, read
    from db2sql.inc as data: its here-documents, its $animals rows, the
    essay and picture files) made in schema XMLSERVTST with `as4tainr
    sql` (test_90000 makes them through XMLSERVICE in one job)."""
    d = os.path.join(clone, 'test', 'php')
    inc = open(os.path.join(d, 'db2sql.inc'), encoding='latin-1').read()
    docs = dict(re.findall(r'\$(\w+) = <<<\w+\n(.*?)\n\w+;', inc, re.S))
    rows = re.findall(r"array\((\d+),\s*'([^']*)',\s*'([^']*)',\s*([\d.]+),\s*([\d.]+)\)", inc)
    essay = open(os.path.join(d, 'ZendEssay.txt'), encoding='latin-1').read()
    picture = open(os.path.join(d, 'ZendStar.png'), 'rb').read().hex().upper()
    q = lambda v: "'" + v.replace("'", "''") + "'"

    def sql(stmt):
        subprocess.run([binary, '--libl', 'XMLSERVTST,QGPL', 'sql', '-'],
                       input=stmt, capture_output=True, text=True, timeout=300)

    sql('CREATE SCHEMA XMLSERVTST')
    for k in ['db2_drp_animal', 'db2_crt_animal']:
        sql(docs[k])
    for r in rows:
        sql(f"INSERT INTO animal (id, breed, name, weight, height) VALUES ({r[0]}, {q(r[1])}, {q(r[2])}, {r[3]}, {r[4]})")
    for k in ['db2_drp_animal1', 'db2_crt_animal1']:
        sql(docs[k])
    for r in rows:
        sql(f"INSERT INTO animal1 (id, essay, picture) VALUES ({r[0]}, {q(essay)}, X'{picture}')")
    for k in ['db2_drp_sp1', 'db2_crt_sp1', 'db2_drp_sp2', 'db2_crt_sp2',
              'db2_idrp_animal2', 'db2_fkdrp_animal2', 'db2_drp_animal2',
              'db2_crt_animal2', 'db2_icrt_animal2', 'db2_fkcrt_animal2']:
        if k in docs:
            sql(docs[k])


def run(binary, proc, xml, ctl):
    q = lambda s: s.replace("'", "''")
    # iPLUGR*: a result set, no output parameter.
    out = '' if proc.startswith('IPLUGR') else ', ?'
    sql = f"CALL XMLSERVICE.{proc}('*NA', '{q(ctl)}', '{q(xml)}'{out})"
    try:
        # The SQL tests find their tables through the library list.
        libl = 'XMLSERVTST,QGPL' if '<sql' in xml else 'QGPL'
        r = subprocess.run([binary, '--libl', libl, 'sql', '--json', '-'],
                           input=sql, capture_output=True, text=True,
                           timeout=300)
        return (r.stdout + r.stderr).strip()
    except subprocess.TimeoutExpired:
        return 'TIMEOUT'


def xml_of(out):
    """The XML in as4tainr sql --json's output: the output parameter
    (message "Procedure X ran: CO = <xml>.") or the result set's rows
    joined, up to the first null character."""
    try:
        v = json.loads(out)
    except ValueError:
        return ''
    if 'rows' in v:
        return ''.join(r[0] or '' for r in v['rows']).split('\0')[0]
    m = re.search(r'ran: C[O0] = (.*)\.\s*$', v.get('message', ''), flags=re.S)
    return m.group(1) if m else ''


def verdict(out, checks):
    if out == 'TIMEOUT' or 'as4tainr:' in out:
        return 'fail', out.splitlines()[-1][:200] if out else ''
    x = xml_of(out)
    if '</hack>' in x:
        x = x[:x.find('</hack>')]
    x = x.strip()
    if not x:
        return 'fail', 'no XML returned'
    if checks['parse']:
        try:
            ET.fromstring(x.encode('utf-8'))
        except ET.ParseError as e:
            return 'fail', f'XML does not parse: {e}'
    absent = checks['absent']
    if not absent:
        # No check of its own: XMLSERVICE's <error> elements stand for the
        # values the test checks through XPath; a delete the script does
        # first (of what an earlier run made) may fail.
        absent = ['<error']
        x = re.sub(r"<cmd[^>]*>\s*<error>[^<]*\bDLT\w+ [^<]*</error>\s*<error>[^<]*</error>\s*</cmd>", '', x)
    for a in absent:
        if a in x:
            m = re.search(r'<xmlerrmsg><!\[CDATA\[(.*?)\]\]>', x)
            i = x.find(a)
            return 'fail', f'{a!r}: ' + (m.group(1) if m else x[max(0, i - 60):i + 60].replace('\n', ' '))
    for p in checks['present']:
        if p not in x:
            return 'fail', f'{p!r} is not there'
    return 'pass', ''


def main():
    args = sys.argv[1:]
    verbose = '-v' in args
    only = args[args.index('-k') + 1:] if '-k' in args else []
    clone = args[0]
    binary = os.environ.get('AS4TAINR_BIN', 'target/debug/as4tainr')
    n = passed = 0
    sql_setup(clone, binary)
    for name, proc, xml, ctl, checks in tests(clone):
        if only and not any(o in name for o in only):
            continue
        v, why = verdict(run(binary, proc, xml, ctl), checks)
        n += 1
        passed += v == 'pass'
        if v != 'pass' or verbose:
            print(v, name, why)
    print(f'XMLSERVICE tests: {passed} of {n} pass')


if __name__ == '__main__':
    main()
