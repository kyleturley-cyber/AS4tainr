//! Output files (OUTPUT(*OUTFILE)): the system's model files in QSYS,
//! made at catalog initialization, and writing a command's records into
//! the file OUTFILE names (made like the model when it is not there).
//!
//! The models' record formats, field names, types and lengths are IBM
//! i 7.5's, from DSPFFD of QSYS/QADSPOBJ and QSYS/QAFDMBRL on PUB400
//! (2026-10-26, tools/golden/verify1.txt v_ffd_objd, v_ffd_mbrl) and of
//! QADSPFFD, QADSPDBR, QAFDBASI, QAFDMBR, QAFDPHY, QAFDACCP and QAFDRFMT
//! (verify2.txt m_*); field texts and column headings are not copied
//! (clean room). Only DSPOBJD and DSPFD TYPE(*MBRLIST) write records yet;
//! the other models are there for programs that declare files LIKE them.

use anyhow::{bail, Result};
use tokio_postgres::Client;

use crate::catalog::{self, ident};

/// A field: name, DDS data type (A, P, S; V: A with VARLEN, which DSPFFD
/// shows as CHAR with a buffer two bytes longer), length, decimal
/// positions.
type Field = (&'static str, char, u32, u32);

/// A model file in QSYS.
pub struct Model {
    pub file: &'static str,
    pub format: &'static str,
    pub text: &'static str,
    pub fields: &'static [Field],
}

/// DSPOBJD OUTPUT(*OUTFILE).
pub const QADSPOBJ: Model = Model {
    file: "QADSPOBJ",
    format: "QLIDOBJD",
    text: "Model output file for DSPOBJD",
    fields: QLIDOBJD,
};

/// DSPFD TYPE(*MBRLIST) OUTPUT(*OUTFILE).
pub const QAFDMBRL: Model = Model {
    file: "QAFDMBRL",
    format: "QWHFDML",
    text: "Model output file for DSPFD TYPE(*MBRLIST)",
    fields: QWHFDML,
};

const QWHFDML: &[Field] = &[
    ("MLRCEN", 'A', 1, 0),
    ("MLRDAT", 'A', 6, 0),
    ("MLRTIM", 'A', 6, 0),
    ("MLFILE", 'A', 10, 0),
    ("MLLIB", 'A', 10, 0),
    ("MLFTYP", 'A', 1, 0),
    ("MLFILA", 'A', 4, 0),
    ("MLMXD", 'A', 3, 0),
    ("MLFATR", 'A', 6, 0),
    ("MLSYSN", 'A', 8, 0),
    ("MLASP", 'P', 3, 0),
    ("MLRES", 'A', 4, 0),
    ("MLNOMB", 'P', 5, 0),
    ("MLNAME", 'A', 10, 0),
    ("MLNRCD", 'P', 10, 0),
    ("MLNDTR", 'P', 10, 0),
    ("MLSIZE", 'P', 10, 0),
    ("MLSEU", 'A', 4, 0),
    ("MLCCEN", 'A', 1, 0),
    ("MLCDAT", 'A', 6, 0),
    ("MLCHGC", 'A', 1, 0),
    ("MLCHGD", 'A', 6, 0),
    ("MLCHGT", 'A', 6, 0),
    ("MLMTXT", 'A', 50, 0),
    ("MLSEU2", 'A', 10, 0),
    ("MLUCEN", 'A', 1, 0),
    ("MLUDAT", 'A', 6, 0),
    ("MLUCNT", 'P', 5, 0),
    ("MLTCEN", 'A', 1, 0),
    ("MLTDAT", 'A', 6, 0),
    ("MLSIZ2", 'P', 15, 0),
    ("MLSTFR", 'A', 1, 0),
];
const QLIDOBJD: &[Field] = &[
    ("ODDCEN", 'A', 1, 0),
    ("ODDDAT", 'A', 6, 0),
    ("ODDTIM", 'A', 6, 0),
    ("ODLBNM", 'A', 10, 0),
    ("ODOBNM", 'A', 10, 0),
    ("ODOBTP", 'A', 8, 0),
    ("ODOBAT", 'A', 10, 0),
    ("ODOBFR", 'A', 1, 0),
    ("ODOBSZ", 'P', 10, 0),
    ("ODOBTX", 'A', 50, 0),
    ("ODOBLK", 'A', 1, 0),
    ("ODOBDM", 'A', 1, 0),
    ("ODCCEN", 'A', 1, 0),
    ("ODCDAT", 'A', 6, 0),
    ("ODCTIM", 'A', 6, 0),
    ("ODOBOW", 'A', 10, 0),
    ("ODSCEN", 'A', 1, 0),
    ("ODSDAT", 'A', 6, 0),
    ("ODSTIM", 'A', 6, 0),
    ("ODSCMD", 'A', 10, 0),
    ("ODSSZE", 'P', 10, 0),
    ("ODSSLT", 'S', 2, 0),
    ("ODSDEV", 'A', 10, 0),
    ("ODSV01", 'A', 6, 0),
    ("ODSV02", 'A', 6, 0),
    ("ODSV03", 'A', 6, 0),
    ("ODSV04", 'A', 6, 0),
    ("ODSV05", 'A', 6, 0),
    ("ODSV06", 'A', 6, 0),
    ("ODSV07", 'A', 6, 0),
    ("ODSV08", 'A', 6, 0),
    ("ODSV09", 'A', 6, 0),
    ("ODSV10", 'A', 6, 0),
    ("ODSVMR", 'A', 1, 0),
    ("ODRCEN", 'A', 1, 0),
    ("ODRDAT", 'A', 6, 0),
    ("ODRTIM", 'A', 6, 0),
    ("ODCPFL", 'A', 6, 0),
    ("ODSRCF", 'A', 10, 0),
    ("ODSRCL", 'A', 10, 0),
    ("ODSRCM", 'A', 10, 0),
    ("ODSRCC", 'A', 1, 0),
    ("ODSRCD", 'A', 6, 0),
    ("ODSRCT", 'A', 6, 0),
    ("ODCMNM", 'A', 7, 0),
    ("ODCMVR", 'A', 6, 0),
    ("ODOBLV", 'A', 8, 0),
    ("ODUMOD", 'A', 1, 0),
    ("ODPPNM", 'A', 7, 0),
    ("ODPPVR", 'A', 6, 0),
    ("ODPCNR", 'A', 5, 0),
    ("ODAPAR", 'A', 6, 0),
    ("ODSSQN", 'S', 4, 0),
    ("ODLCEN", 'A', 1, 0),
    ("ODLDAT", 'A', 6, 0),
    ("ODLTIM", 'A', 6, 0),
    ("ODSFIL", 'A', 10, 0),
    ("ODSFLB", 'A', 10, 0),
    ("ODASP", 'S', 2, 0),
    ("ODLBL", 'A', 17, 0),
    ("ODPTFN", 'A', 7, 0),
    ("ODOBSY", 'A', 8, 0),
    ("ODCRTU", 'A', 10, 0),
    ("ODCRTS", 'A', 8, 0),
    ("ODUUPD", 'A', 1, 0),
    ("ODUCEN", 'A', 1, 0),
    ("ODUDAT", 'A', 6, 0),
    ("ODUCNT", 'P', 5, 0),
    ("ODTCEN", 'A', 1, 0),
    ("ODTDAT", 'A', 6, 0),
    ("ODODMN", 'A', 2, 0),
    ("ODCPVR", 'A', 6, 0),
    ("ODCVRM", 'A', 6, 0),
    ("ODPVRM", 'A', 6, 0),
    ("ODCPRS", 'A', 1, 0),
    ("ODOASP", 'A', 1, 0),
    ("ODAAPI", 'A', 1, 0),
    ("ODAPIC", 'A', 1, 0),
    ("ODUATR", 'A', 10, 0),
    ("ODACEN", 'A', 1, 0),
    ("ODADAT", 'A', 6, 0),
    ("ODATIM", 'A', 6, 0),
    ("ODAUDT", 'A', 10, 0),
    ("ODSIZU", 'P', 10, 0),
    ("ODBPUN", 'P', 10, 0),
    ("ODPGP", 'A', 10, 0),
    ("ODSSQL", 'S', 10, 0),
    ("ODOSIG", 'A', 1, 0),
    ("ODJRST", 'A', 1, 0),
    ("ODJRNM", 'A', 10, 0),
    ("ODJRLB", 'A', 10, 0),
    ("ODJRIM", 'A', 1, 0),
    ("ODJREN", 'A', 1, 0),
    ("ODJRCN", 'A', 1, 0),
    ("ODJRDT", 'A', 6, 0),
    ("ODJRTI", 'A', 6, 0),
    ("ODSSZU", 'P', 10, 0),
    ("ODSBPU", 'P', 10, 0),
    ("ODASPL", 'S', 5, 0),
    ("ODLASN", 'S', 5, 0),
    ("ODOADN", 'A', 10, 0),
    ("ODLADN", 'A', 10, 0),
    ("ODSSIG", 'A', 1, 0),
    ("ODMSIG", 'A', 1, 0),
    ("ODASSZ", 'P', 15, 0),
    ("ODALGN", 'A', 1, 0),
    ("ODOADG", 'A', 10, 0),
    ("ODLADG", 'A', 10, 0),
    ("ODSJRA", 'A', 10, 0),
    ("ODSJRL", 'A', 10, 0),
    ("ODSJRD", 'A', 10, 0),
    ("ODSJRG", 'A', 10, 0),
    ("ODJRJF", 'A', 1, 0),
    ("ODDTL", 'A', 10, 0),
    ("ODAUTC", 'A', 10, 0),
    ("ODBDID", 'A', 16, 0),
];

/// DSPFFD OUTPUT(*OUTFILE). (PUB400 2026-10-26, verify2 m_ffd: 102 fields, 1066 bytes.)
pub const QADSPFFD: Model = Model {
    file: "QADSPFFD",
    format: "QWHDRFFD",
    text: "Model output file for DSPFFD",
    fields: QWHDRFFD,
};

const QWHDRFFD: &[Field] = &[
    ("WHFILE", 'A', 10, 0),
    ("WHLIB", 'A', 10, 0),
    ("WHCRTD", 'A', 7, 0),
    ("WHFTYP", 'A', 1, 0),
    ("WHCNT", 'S', 5, 0),
    ("WHDTTM", 'A', 13, 0),
    ("WHNAME", 'A', 10, 0),
    ("WHSEQ", 'A', 13, 0),
    ("WHTEXT", 'A', 50, 0),
    ("WHFLDN", 'S', 5, 0),
    ("WHRLEN", 'S', 5, 0),
    ("WHFLDI", 'A', 10, 0),
    ("WHFLDE", 'A', 10, 0),
    ("WHFOBO", 'S', 5, 0),
    ("WHIBO", 'S', 5, 0),
    ("WHFLDB", 'S', 5, 0),
    ("WHFLDD", 'S', 2, 0),
    ("WHFLDP", 'S', 2, 0),
    ("WHFTXT", 'A', 50, 0),
    ("WHRCDE", 'S', 3, 0),
    ("WHRFIL", 'A', 10, 0),
    ("WHRLIB", 'A', 10, 0),
    ("WHRFMT", 'A', 10, 0),
    ("WHRFLD", 'A', 10, 0),
    ("WHCHD1", 'A', 20, 0),
    ("WHCHD2", 'A', 20, 0),
    ("WHCHD3", 'A', 20, 0),
    ("WHFLDT", 'A', 1, 0),
    ("WHFIOB", 'A', 1, 0),
    ("WHECDE", 'A', 2, 0),
    ("WHEWRD", 'A', 32, 0),
    ("WHVCNE", 'S', 4, 0),
    ("WHNFLD", 'S', 5, 0),
    ("WHNIND", 'S', 2, 0),
    ("WHSHFT", 'A', 1, 0),
    ("WHALTY", 'A', 1, 0),
    ("WHALIS", 'A', 30, 0),
    ("WHJREF", 'S', 2, 0),
    ("WHDFTL", 'S', 2, 0),
    ("WHDFT", 'A', 30, 0),
    ("WHCHRI", 'A', 1, 0),
    ("WHCTNT", 'A', 1, 0),
    ("WHFONT", 'A', 10, 0),
    ("WHCSWD", 'S', 3, 1),
    ("WHCSHI", 'S', 3, 1),
    ("WHBCNM", 'A', 10, 0),
    ("WHBCHI", 'S', 3, 1),
    ("WHMAP", 'A', 1, 0),
    ("WHMAPS", 'S', 5, 0),
    ("WHMAPL", 'S', 5, 0),
    ("WHSYSN", 'A', 8, 0),
    ("WHRES1", 'A', 2, 0),
    ("WHSQLT", 'A', 1, 0),
    ("WHHEX", 'A', 1, 0),
    ("WHPNTS", 'P', 4, 1),
    ("WHCSID", 'P', 5, 0),
    ("WHFMT", 'A', 4, 0),
    ("WHSEP", 'A', 1, 0),
    ("WHVARL", 'A', 1, 0),
    ("WHALLC", 'P', 5, 0),
    ("WHNULL", 'A', 1, 0),
    ("WHFCSN", 'A', 10, 0),
    ("WHFCSL", 'A', 10, 0),
    ("WHFCPN", 'A', 10, 0),
    ("WHFCPL", 'A', 10, 0),
    ("WHCDFN", 'A', 10, 0),
    ("WHCDFL", 'A', 10, 0),
    ("WHDCDF", 'A', 10, 0),
    ("WHDCDL", 'A', 10, 0),
    ("WHTXRT", 'P', 3, 0),
    ("WHFLDG", 'S', 5, 0),
    ("WHFDSL", 'S', 5, 0),
    ("WHFSPS", 'P', 4, 1),
    ("WHCFPS", 'P', 4, 1),
    ("WHIFPS", 'P', 4, 1),
    ("WHDBLL", 'P', 10, 0),
    ("WHDBUN", 'A', 128, 0),
    ("WHDBUL", 'A', 10, 0),
    ("WHDBFC", 'A', 1, 0),
    ("WHDBFI", 'A', 1, 0),
    ("WHDBRP", 'A', 1, 0),
    ("WHDBWP", 'A', 1, 0),
    ("WHDBRC", 'A', 1, 0),
    ("WHDBOU", 'A', 1, 0),
    ("WHPSUD", 'A', 1, 0),
    ("WHBCUH", 'S', 5, 2),
    ("WHFPSW", 'P', 4, 1),
    ("WHFSPW", 'P', 4, 1),
    ("WHCFPW", 'P', 4, 1),
    ("WHIFPW", 'P', 4, 1),
    ("WHRWID", 'A', 1, 0),
    ("WHIDC", 'A', 1, 0),
    ("WHDROW", 'S', 3, 0),
    ("WHDCOL", 'S', 3, 0),
    ("WHALI2", 'A', 258, 0),
    ("WHALCH", 'A', 1, 0),
    ("WHNRML", 'A', 1, 0),
    ("WHJRF2", 'S', 3, 0),
    ("WHHDNCOL", 'A', 1, 0),
    ("WHRCTS", 'A', 1, 0),
    ("WHFPPN", 'A', 10, 0),
    ("WHFPLN", 'A', 10, 0),
];

/// DSPDBR OUTPUT(*OUTFILE). (PUB400 2026-10-26, verify2 m_dbr: 16 fields, 372 bytes.)
pub const QADSPDBR: Model = Model {
    file: "QADSPDBR",
    format: "QWHDRDBR",
    text: "Model output file for DSPDBR",
    fields: QWHDRDBR,
};

const QWHDRDBR: &[Field] = &[
    ("WHRTYP", 'A', 1, 0),
    ("WHRFI", 'A', 10, 0),
    ("WHRLI", 'A', 10, 0),
    ("WHRMB", 'A', 10, 0),
    ("WHRRD", 'A', 10, 0),
    ("WHNO", 'S', 5, 0),
    ("WHDTM", 'A', 13, 0),
    ("WHREFI", 'A', 10, 0),
    ("WHRELI", 'A', 10, 0),
    ("WHREMB", 'A', 10, 0),
    ("WHTYPE", 'A', 1, 0),
    ("WHJDIL", 'S', 2, 0),
    ("WHJREF", 'S', 2, 0),
    ("WHSYSN", 'A', 8, 0),
    ("WHCTLN", 'A', 10, 0),
    ("WHCSTN", 'V', 258, 0),
];

/// DSPFD TYPE(*BASATR) OUTPUT(*OUTFILE). (PUB400 2026-10-26, verify2 m_basi: 48 fields, 199 bytes.)
pub const QAFDBASI: Model = Model {
    file: "QAFDBASI",
    format: "QWHFDBAS",
    text: "Model output file for DSPFD TYPE(*BASATR)",
    fields: QWHFDBAS,
};

const QWHFDBAS: &[Field] = &[
    ("ATRCEN", 'A', 1, 0),
    ("ATRDAT", 'A', 6, 0),
    ("ATRTIM", 'A', 6, 0),
    ("ATFILE", 'A', 10, 0),
    ("ATLIB", 'A', 10, 0),
    ("ATFTYP", 'A', 1, 0),
    ("ATFILA", 'A', 4, 0),
    ("ATMXDD", 'A', 1, 0),
    ("ATMXDC", 'A', 1, 0),
    ("ATMXDB", 'A', 1, 0),
    ("ATFATR", 'A', 6, 0),
    ("ATSYSN", 'A', 8, 0),
    ("ATASP", 'P', 3, 0),
    ("ATRES", 'A', 4, 0),
    ("ATDTAT", 'A', 1, 0),
    ("ATWAIT", 'P', 5, 0),
    ("ATWATR", 'P', 5, 0),
    ("ATSHAR", 'A', 1, 0),
    ("ATLVLC", 'A', 1, 0),
    ("ATTXT", 'A', 50, 0),
    ("ATNOFM", 'P', 5, 0),
    ("ATFCCN", 'A', 1, 0),
    ("ATFCDT", 'A', 6, 0),
    ("ATFCTM", 'A', 6, 0),
    ("ATFLS", 'A', 1, 0),
    ("ATICAP", 'A', 1, 0),
    ("ATRES2", 'A', 9, 0),
    ("ATAQDV", 'A', 10, 0),
    ("ATMXDV", 'P', 3, 0),
    ("ATSPOL", 'A', 1, 0),
    ("ATNODV", 'P', 3, 0),
    ("ATUBL", 'P', 5, 0),
    ("ATIDTA", 'A', 1, 0),
    ("ATRES3", 'A', 9, 0),
    ("ATACCP", 'A', 1, 0),
    ("ATSELO", 'A', 1, 0),
    ("ATCSEQ", 'A', 1, 0),
    ("ATNOMB", 'P', 5, 0),
    ("ATJOIN", 'A', 1, 0),
    ("ATSQLT", 'A', 1, 0),
    ("ATRES4", 'A', 8, 0),
    ("ATMQMT", 'A', 1, 0),
    ("ATMQRF", 'A', 1, 0),
    ("ATMQOE", 'A', 1, 0),
    ("ATMQCL", 'A', 2, 0),
    ("ATMQRS", 'A', 1, 0),
    ("ATDFRM", 'A', 1, 0),
    ("ATDFWN", 'A', 1, 0),
];

/// DSPFD TYPE(*MBR) OUTPUT(*OUTFILE). (PUB400 2026-10-26, verify2 m_mbr: 155 fields, 906 bytes.)
pub const QAFDMBR: Model = Model {
    file: "QAFDMBR",
    format: "QWHFDMBR",
    text: "Model output file for DSPFD TYPE(*MBR)",
    fields: QWHFDMBR,
};

const QWHFDMBR: &[Field] = &[
    ("MBRCEN", 'A', 1, 0),
    ("MBRDAT", 'A', 6, 0),
    ("MBRTIM", 'A', 6, 0),
    ("MBFILE", 'A', 10, 0),
    ("MBLIB", 'A', 10, 0),
    ("MBFTYP", 'A', 1, 0),
    ("MBFILA", 'A', 4, 0),
    ("MBMXD", 'A', 3, 0),
    ("MBFATR", 'A', 6, 0),
    ("MBSYSN", 'A', 8, 0),
    ("MBASP", 'P', 3, 0),
    ("MBRES", 'A', 4, 0),
    ("MBDTAT", 'A', 1, 0),
    ("MBWAIT", 'P', 5, 0),
    ("MBWATR", 'P', 5, 0),
    ("MBSHAR", 'A', 1, 0),
    ("MBLVLC", 'A', 1, 0),
    ("MBTXT", 'A', 50, 0),
    ("MBNOFM", 'P', 5, 0),
    ("MBFCCN", 'A', 1, 0),
    ("MBFCDT", 'A', 6, 0),
    ("MBFCTM", 'A', 6, 0),
    ("MBFLS", 'A', 1, 0),
    ("MBICAP", 'A', 1, 0),
    ("MBRES2", 'A', 9, 0),
    ("MBACCP", 'A', 1, 0),
    ("MBSELO", 'A', 1, 0),
    ("MBCSEQ", 'A', 1, 0),
    ("MBNOMB", 'P', 5, 0),
    ("MBJOIN", 'A', 1, 0),
    ("MBRES4", 'A', 9, 0),
    ("MBNAME", 'A', 10, 0),
    ("MBCCEN", 'A', 1, 0),
    ("MBCDAT", 'A', 6, 0),
    ("MBCTIM", 'A', 6, 0),
    ("MBECEN", 'A', 1, 0),
    ("MBEDAT", 'A', 6, 0),
    ("MBMTXT", 'A', 50, 0),
    ("MBAPFI", 'A', 10, 0),
    ("MBAPLB", 'A', 10, 0),
    ("MBAPMB", 'A', 10, 0),
    ("MBMAXM", 'P', 5, 0),
    ("MBMANT", 'A', 1, 0),
    ("MBRECV", 'A', 1, 0),
    ("MBFKAP", 'A', 1, 0),
    ("MBMXKL", 'P', 3, 0),
    ("MBMXRL", 'P', 5, 0),
    ("MBJRNL", 'A', 1, 0),
    ("MBJRNM", 'A', 10, 0),
    ("MBJRLB", 'A', 10, 0),
    ("MBJRIM", 'A', 1, 0),
    ("MBJRSC", 'A', 1, 0),
    ("MBJRSD", 'A', 6, 0),
    ("MBJRST", 'A', 6, 0),
    ("MBSIZ", 'P', 10, 0),
    ("MBSIZI", 'P', 5, 0),
    ("MBSIZM", 'P', 5, 0),
    ("MBCURI", 'P', 10, 0),
    ("MBRCDC", 'P', 10, 0),
    ("MBNRCD", 'P', 10, 0),
    ("MBNDTR", 'P', 10, 0),
    ("MBALLO", 'A', 1, 0),
    ("MBCONT", 'A', 1, 0),
    ("MBUNIT", 'P', 3, 0),
    ("MBFMTS", 'A', 10, 0),
    ("MBFMSL", 'A', 10, 0),
    ("MBFRCR", 'P', 5, 0),
    ("MBRSHR", 'A', 1, 0),
    ("MBDLTP", 'P', 3, 0),
    ("MBDSSZ", 'P', 10, 0),
    ("MBISIZ", 'P', 10, 0),
    ("MBIXNT", 'P', 10, 0),
    ("MBNACC", 'P', 10, 0),
    ("MBSEU", 'A', 4, 0),
    ("MBCHGC", 'A', 1, 0),
    ("MBCHGD", 'A', 6, 0),
    ("MBCHGT", 'A', 6, 0),
    ("MBUPDC", 'A', 1, 0),
    ("MBUPDD", 'A', 6, 0),
    ("MBUPDT", 'A', 6, 0),
    ("MBEXDC", 'A', 1, 0),
    ("MBEXDD", 'A', 6, 0),
    ("MBEXDT", 'A', 6, 0),
    ("MBLEC", 'A', 1, 0),
    ("MBLED", 'A', 6, 0),
    ("MBLET", 'A', 6, 0),
    ("MBSAVC", 'A', 1, 0),
    ("MBSAVD", 'A', 6, 0),
    ("MBSAVT", 'A', 6, 0),
    ("MBRSTC", 'A', 1, 0),
    ("MBRSTD", 'A', 6, 0),
    ("MBRSTT", 'A', 6, 0),
    ("MBNSCM", 'P', 3, 0),
    ("MBBOF", 'A', 10, 0),
    ("MBBOL", 'A', 10, 0),
    ("MBBOM", 'A', 10, 0),
    ("MBBOLF", 'A', 10, 0),
    ("MBBOR", 'P', 10, 0),
    ("MBBOMA", 'P', 10, 0),
    ("MBJROM", 'A', 1, 0),
    ("MBIST", 'A', 1, 0),
    ("MBISF", 'A', 10, 0),
    ("MBISL", 'A', 10, 0),
    ("MBISM", 'A', 10, 0),
    ("MBISMT", 'A', 1, 0),
    ("MBISRV", 'A', 1, 0),
    ("MBISFK", 'A', 1, 0),
    ("MBISUN", 'A', 1, 0),
    ("MBACPJ", 'A', 1, 0),
    ("MBALRD", 'A', 1, 0),
    ("MBALWT", 'A', 1, 0),
    ("MBALUP", 'A', 1, 0),
    ("MBALDT", 'A', 1, 0),
    ("MBSEU2", 'A', 10, 0),
    ("MBUCEN", 'A', 1, 0),
    ("MBUDAT", 'A', 6, 0),
    ("MBUCNT", 'P', 5, 0),
    ("MBTCEN", 'A', 1, 0),
    ("MBTDAT", 'A', 6, 0),
    ("MBINDX", 'A', 1, 0),
    ("MBDSZ2", 'P', 15, 0),
    ("MBMXK2", 'P', 5, 0),
    ("MBOPOP", 'P', 20, 0),
    ("MBCLOP", 'P', 20, 0),
    ("MBWROP", 'P', 20, 0),
    ("MBUPOP", 'P', 20, 0),
    ("MBDLOP", 'P', 20, 0),
    ("MBLRDS", 'P', 20, 0),
    ("MBPRDS", 'P', 20, 0),
    ("MBCROP", 'P', 20, 0),
    ("MBDSCP", 'P', 20, 0),
    ("MBRGOP", 'P', 20, 0),
    ("MBAPBL", 'P', 20, 0),
    ("MBRJKY", 'P', 20, 0),
    ("MBRJNK", 'P', 20, 0),
    ("MBRJGR", 'P', 20, 0),
    ("MBACLR", 'P', 20, 0),
    ("MBACPR", 'P', 20, 0),
    ("MBISZ2", 'P', 15, 0),
    ("MBSTFR", 'A', 1, 0),
    ("MBUEV1", 'P', 15, 0),
    ("MBUKV1", 'P', 15, 0),
    ("MBUKV2", 'P', 15, 0),
    ("MBUKV3", 'P', 15, 0),
    ("MBUKV4", 'P', 15, 0),
    ("MBIOVF", 'P', 15, 0),
    ("MBINTR", 'A', 1, 0),
    ("MBINJR", 'A', 10, 0),
    ("MBINJL", 'A', 10, 0),
    ("MBIASP", 'A', 10, 0),
    ("MBRBCK", 'A', 1, 0),
    ("MBMRCN", 'A', 1, 0),
    ("MBMRDT", 'A', 6, 0),
    ("MBMRTM", 'A', 6, 0),
    ("MBNSC2", 'P', 5, 0),
];

/// DSPFD TYPE(*ATR) FILEATR(*PF) OUTPUT(*OUTFILE). (PUB400 2026-10-26, verify2 m_phy: 98 fields, 3992 bytes.)
pub const QAFDPHY: Model = Model {
    file: "QAFDPHY",
    format: "QWHFDPHY",
    text: "Model output file for DSPFD TYPE(*ATR) of physical files",
    fields: QWHFDPHY,
};

const QWHFDPHY: &[Field] = &[
    ("PHRCEN", 'A', 1, 0),
    ("PHRDAT", 'A', 6, 0),
    ("PHRTIM", 'A', 6, 0),
    ("PHFILE", 'A', 10, 0),
    ("PHLIB", 'A', 10, 0),
    ("PHFTYP", 'A', 1, 0),
    ("PHFILA", 'A', 4, 0),
    ("PHMXD", 'A', 3, 0),
    ("PHFATR", 'A', 6, 0),
    ("PHSYSN", 'A', 8, 0),
    ("PHASP", 'P', 3, 0),
    ("PHRES", 'A', 4, 0),
    ("PHDTAT", 'A', 1, 0),
    ("PHWAIT", 'P', 5, 0),
    ("PHWATR", 'P', 5, 0),
    ("PHSHAR", 'A', 1, 0),
    ("PHLVLC", 'A', 1, 0),
    ("PHTXT", 'A', 50, 0),
    ("PHNOFM", 'P', 5, 0),
    ("PHFCCN", 'A', 1, 0),
    ("PHFCDT", 'A', 6, 0),
    ("PHFCTM", 'A', 6, 0),
    ("PHFLS", 'A', 1, 0),
    ("PHICAP", 'A', 1, 0),
    ("PHRES2", 'A', 9, 0),
    ("PHACCP", 'A', 1, 0),
    ("PHSELO", 'A', 1, 0),
    ("PHCSEQ", 'A', 1, 0),
    ("PHNOMB", 'P', 5, 0),
    ("PHRES3", 'A', 1, 0),
    ("PHSQLT", 'A', 1, 0),
    ("PHRES4", 'A', 8, 0),
    ("PHMAXM", 'P', 5, 0),
    ("PHMANT", 'A', 1, 0),
    ("PHRECV", 'A', 1, 0),
    ("PHFKAP", 'A', 1, 0),
    ("PHSIZ", 'P', 10, 0),
    ("PHSIZI", 'P', 5, 0),
    ("PHSIZM", 'P', 5, 0),
    ("PHRCDC", 'P', 10, 0),
    ("PHALLO", 'A', 1, 0),
    ("PHCONT", 'A', 1, 0),
    ("PHUNIT", 'P', 3, 0),
    ("PHFRCR", 'P', 5, 0),
    ("PHDLTP", 'P', 3, 0),
    ("PHMXKL", 'P', 3, 0),
    ("PHMXRL", 'P', 5, 0),
    ("PHJRNL", 'A', 1, 0),
    ("PHJRNM", 'A', 10, 0),
    ("PHJRLB", 'A', 10, 0),
    ("PHJRIM", 'A', 1, 0),
    ("PHJRSC", 'A', 1, 0),
    ("PHJRSD", 'A', 6, 0),
    ("PHJRST", 'A', 6, 0),
    ("PHJROM", 'A', 1, 0),
    ("PHNAML", 'P', 3, 0),
    ("PHRMTN", 'A', 50, 0),
    ("PHRDLT", 'A', 1, 0),
    ("PHNACM", 'P', 3, 0),
    ("PHACCM", 'A', 14, 0),
    ("PHALRD", 'A', 1, 0),
    ("PHALWT", 'A', 1, 0),
    ("PHALUP", 'A', 1, 0),
    ("PHALDT", 'A', 1, 0),
    ("PHDCTL", 'A', 1, 0),
    ("PHINID", 'S', 11, 0),
    ("PHDCTN", 'A', 10, 0),
    ("PHFILN", 'A', 10, 0),
    ("PHRUSE", 'A', 1, 0),
    ("PHCSID", 'P', 5, 0),
    ("PHMXK2", 'P', 5, 0),
    ("PHSSEQ", 'A', 10, 0),
    ("PHSSLB", 'A', 10, 0),
    ("PHLGID", 'A', 10, 0),
    ("PHTBSH", 'A', 1, 0),
    ("PHTBSU", 'A', 1, 0),
    ("PHNOCS", 'P', 5, 0),
    ("PHNOTR", 'P', 5, 0),
    ("PHTBLN", 'P', 5, 0),
    ("PHLNTB", 'V', 258, 0),
    ("PHAPSZ", 'A', 1, 0),
    ("PHDSTF", 'A', 1, 0),
    ("PHNODG", 'A', 10, 0),
    ("PHNODL", 'A', 10, 0),
    ("PHNUMP", 'P', 4, 0),
    ("PHPRTK", 'V', 3300, 0),
    ("PHFLPS", 'A', 1, 0),
    ("PHPRBY", 'A', 1, 0),
    ("PHMQMT", 'A', 1, 0),
    ("PHMQRF", 'A', 1, 0),
    ("PHMQOE", 'A', 1, 0),
    ("PHMQCL", 'A', 2, 0),
    ("PHAPGS", 'P', 5, 0),
    ("PHVOLT", 'A', 1, 0),
    ("PHMQRS", 'A', 1, 0),
    ("PHMQRM", 'A', 1, 0),
    ("PHMQWN", 'A', 1, 0),
    ("PHKPIM", 'A', 1, 0),
];

/// DSPFD TYPE(*ACCPTH) OUTPUT(*OUTFILE). (PUB400 2026-10-26, verify2 m_accp: 33 fields, 130 bytes.)
pub const QAFDACCP: Model = Model {
    file: "QAFDACCP",
    format: "QWHFDACP",
    text: "Model output file for DSPFD TYPE(*ACCPTH)",
    fields: QWHFDACP,
};

const QWHFDACP: &[Field] = &[
    ("APRCEN", 'A', 1, 0),
    ("APRDAT", 'A', 6, 0),
    ("APRTIM", 'A', 6, 0),
    ("APFILE", 'A', 10, 0),
    ("APLIB", 'A', 10, 0),
    ("APFTYP", 'A', 1, 0),
    ("APFILA", 'A', 4, 0),
    ("APMXD", 'A', 3, 0),
    ("APFATR", 'A', 6, 0),
    ("APSYSN", 'A', 8, 0),
    ("APASP", 'P', 3, 0),
    ("APRES", 'A', 4, 0),
    ("APMANT", 'A', 1, 0),
    ("APUNIQ", 'A', 1, 0),
    ("APKEYO", 'A', 1, 0),
    ("APSELO", 'A', 1, 0),
    ("APACCP", 'A', 1, 0),
    ("APNSCO", 'P', 3, 0),
    ("APBOF", 'A', 10, 0),
    ("APBOL", 'A', 10, 0),
    ("APBOLF", 'A', 10, 0),
    ("APNKYF", 'P', 3, 0),
    ("APKEYF", 'A', 10, 0),
    ("APKSEQ", 'A', 1, 0),
    ("APKSIN", 'A', 1, 0),
    ("APKZD", 'A', 1, 0),
    ("APKASQ", 'A', 1, 0),
    ("APKEYN", 'P', 3, 0),
    ("APJOIN", 'A', 1, 0),
    ("APACPJ", 'A', 1, 0),
    ("APRIKY", 'A', 1, 0),
    ("APUUIV", 'P', 15, 0),
    ("APNSC2", 'P', 5, 0),
];

/// DSPFD TYPE(*RCDFMT) OUTPUT(*OUTFILE). (PUB400 2026-10-26, verify2 m_rfmt: 53 fields, 290 bytes.)
pub const QAFDRFMT: Model = Model {
    file: "QAFDRFMT",
    format: "QWHFDFMT",
    text: "Model output file for DSPFD TYPE(*RCDFMT)",
    fields: QWHFDFMT,
};

const QWHFDFMT: &[Field] = &[
    ("RFRCEN", 'A', 1, 0),
    ("RFRDAT", 'A', 6, 0),
    ("RFRTIM", 'A', 6, 0),
    ("RFFILE", 'A', 10, 0),
    ("RFLIB", 'A', 10, 0),
    ("RFFTYP", 'A', 1, 0),
    ("RFFILA", 'A', 4, 0),
    ("RFMXDD", 'A', 1, 0),
    ("RFMXDC", 'A', 1, 0),
    ("RFMXDB", 'A', 1, 0),
    ("RFFATR", 'A', 6, 0),
    ("RFSYSN", 'A', 8, 0),
    ("RFASP", 'P', 3, 0),
    ("RFRES", 'A', 4, 0),
    ("RFTOTF", 'P', 4, 0),
    ("RFNAME", 'A', 10, 0),
    ("RFFLDN", 'P', 5, 0),
    ("RFLEN", 'P', 5, 0),
    ("RFID", 'A', 13, 0),
    ("RFTYPE", 'P', 1, 0),
    ("RFASFN", 'A', 10, 0),
    ("RFFTXT", 'A', 50, 0),
    ("RFHIGH", 'P', 4, 0),
    ("RFWIDE", 'P', 5, 0),
    ("RFFONT", 'A', 10, 0),
    ("RFPGRT", 'P', 3, 0),
    ("RFLPI", 'P', 3, 1),
    ("RFCHWD", 'P', 3, 1),
    ("RFCHHI", 'P', 3, 1),
    ("RFPNTS", 'P', 4, 1),
    ("RFWIN", 'A', 1, 0),
    ("RFFCSN", 'A', 10, 0),
    ("RFFCSL", 'A', 10, 0),
    ("RFFCPN", 'A', 10, 0),
    ("RFFCPL", 'A', 10, 0),
    ("RFCDFN", 'A', 10, 0),
    ("RFCDFL", 'A', 10, 0),
    ("RFDCDF", 'A', 10, 0),
    ("RFDCDL", 'A', 10, 0),
    ("RFMNB", 'A', 1, 0),
    ("RFPLD", 'A', 1, 0),
    ("RFGRD", 'A', 1, 0),
    ("RFFSPS", 'P', 4, 1),
    ("RFCFPS", 'P', 4, 1),
    ("RFIFPS", 'P', 4, 1),
    ("RFSPGG", 'A', 1, 0),
    ("RFEPGG", 'A', 1, 0),
    ("RFDITG", 'A', 1, 0),
    ("RFIDTM", 'A', 1, 0),
    ("RFFPSW", 'P', 4, 1),
    ("RFCPSW", 'P', 4, 1),
    ("RFSPSW", 'P', 4, 1),
    ("RFGPSW", 'P', 4, 1),
];

/// The model's DDS source (fixed form: name at column 19, length ending
/// at column 34, type at 35, decimal positions at 36-37).
fn dds(m: &Model) -> String {
    let mut s = format!("     A          R {:<10}\n", m.format);
    for (name, ty, len, dec) in m.fields {
        if *ty == 'V' {
            s.push_str(&format!(
                "     A            {name:<10}{len:>6}A         VARLEN\n"
            ));
            continue;
        }
        let decs = if *ty == 'A' {
            "  ".to_string()
        } else {
            format!("{dec:>2}")
        };
        s.push_str(&format!("     A            {name:<10}{len:>6}{ty}{decs}\n"));
    }
    s
}

/// The model's file description, named `name`.
fn description(m: &Model, name: &str) -> Result<crate::dds::PhysicalFile> {
    let mut pf = crate::dds::parse_pf(name, &dds(m))?;
    pf.text = m.text.to_string();
    Ok(pf)
}

/// At catalog initialization: the model files in QSYS, when missing.
pub async fn init(client: &Client) -> Result<()> {
    for m in [
        &QADSPOBJ, &QAFDMBRL, &QADSPFFD, &QADSPDBR, &QAFDBASI, &QAFDMBR, &QAFDPHY, &QAFDACCP,
        &QAFDRFMT,
    ] {
        let there = client
            .query_opt(
                "SELECT 1 FROM as4tainr.objects WHERE library='QSYS' AND name=$1 AND objtype='*FILE'",
                &[&m.file],
            )
            .await?
            .is_some();
        if there {
            continue;
        }
        let pf = description(m, m.file)?;
        client.batch_execute("BEGIN").await?;
        match catalog::create_physical_file_in(client, "QSYS", m.file, &pf).await {
            Ok(()) => client.batch_execute("COMMIT").await?,
            Err(e) => {
                client.batch_execute("ROLLBACK").await?;
                return Err(e);
            }
        }
    }
    Ok(())
}

/// A record for an output file: values by field name (fields not given
/// are blank or zero).
pub type Record = Vec<(&'static str, String)>;

/// The models' century digit for a four-digit year: 0 for 19xx, 1 for
/// 20xx.
pub fn century(year: &str) -> String {
    if year.starts_with("19") {
        "0".into()
    } else {
        "1".into()
    }
}

/// Write `records` into the output file `outfile` (LIB/FILE, the library
/// *LIBL or *CURLIB too), member option `option` (*REPLACE clears it
/// first, *ADD adds). A file that is not there is made like the model,
/// with the model's record format, and the messages say so: CPF9861 (the
/// file) and CPF9862 (its member, named like the file); an existing file
/// gets none (PUB400 2026-10-26, verify2 o_objd, o_objd_again, o_mbrl).
/// Returns those messages and the file's library and name.
pub async fn write(
    job: &crate::commands::Job<'_>,
    outfile: &str,
    option: &str,
    model: &Model,
    records: &[Record],
) -> Result<Written> {
    let q = outfile.trim().to_ascii_uppercase();
    let (label, name) = match q.split_once('/') {
        Some((l, n)) => (l.to_string(), n.to_string()),
        None => ("*LIBL".to_string(), q.clone()),
    };
    if name.is_empty() || name.starts_with('*') {
        return Err(crate::cl::value_error(
            "OUTFILE needs the name of a database file.",
        ));
    }
    let client = job.client;
    let libs: Vec<String> = match label.as_str() {
        "*LIBL" => job.library_list(),
        "*CURLIB" => vec![job.current_library()],
        l => vec![l.to_string()],
    };
    let found = catalog::resolve_file(client, &libs, &name).await.ok();
    let mut messages = Vec::new();
    let (lib, file) = match found {
        Some(f) => (f.library.clone(), f.file),
        None => {
            let lib = match label.as_str() {
                "*LIBL" | "*CURLIB" => job.current_library(),
                l => l.to_string(),
            };
            if !catalog::library_exists(client, &lib).await? {
                bail!("CPF9810: There is no library {lib}.");
            }
            let pf = description(model, &name)?;
            let mut c = crate::runtime::connect_db(&job.env.database_url).await?;
            catalog::create_physical_file(&mut c, &lib, &pf).await?;
            crate::authority::set_owner(client, &lib, &name, "*FILE", &job.user).await?;
            let shown = crate::qtemp::logical(&lib);
            messages.push(format!(
                "CPF9861: File {name} in {shown} was made to receive the output."
            ));
            messages.push(format!(
                "CPF9862: Output file {name} in {shown} has a new member {name}."
            ));
            (lib, pf)
        }
    };
    let table = format!("{}.{}", ident(&lib), ident(&name));
    if !option.trim().eq_ignore_ascii_case("*ADD") {
        client
            .batch_execute(&format!("DELETE FROM {table}"))
            .await?;
    }
    let cols: Vec<String> = file.fields.iter().map(|f| f.column()).collect();
    let casts: Vec<String> = file
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| format!("CAST(${}::text AS {})", i + 1, f.sql_type()))
        .collect();
    let sql = format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        cols.join(", "),
        casts.join(", ")
    );
    for r in records {
        let values: Vec<String> = file
            .fields
            .iter()
            .map(|f| {
                let given = r
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&f.name))
                    .map(|(_, v)| v.clone());
                if f.data_type == crate::dds::DataType::Char {
                    let v = given.unwrap_or_default();
                    let n = f.length as usize;
                    let mut v: String = v.chars().take(n).collect();
                    while v.chars().count() < n {
                        v.push(' ');
                    }
                    v
                } else {
                    given
                        .filter(|v| !v.trim().is_empty())
                        .unwrap_or_else(|| "0".into())
                }
            })
            .collect();
        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = values
            .iter()
            .map(|v| v as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        client.execute(sql.as_str(), &params).await?;
    }
    Ok(Written {
        messages,
        library: crate::qtemp::logical(&lib),
        file: name,
    })
}

/// What `write` did.
pub struct Written {
    /// CPF9861 and CPF9862 when the file was made.
    pub messages: Vec<String>,
    pub library: String,
    pub file: String,
}

/// The records-added message of DSPFD OUTPUT(*OUTFILE): CPF3030 with
/// the count, sent each time (PUB400 2026-10-26, verify2 o_mbrl and
/// o_mbrl_pf; DSPOBJD sends none, o_objd_add).
pub fn records_added(w: &Written, n: usize) -> String {
    format!(
        "CPF3030: {n} records were added to member {} of file {} in {}.",
        w.file, w.file, w.library
    )
}

/// A date in the job's date format (system value QDATFMT: YMD, DMY,
/// else MDY), as six digits: DSPOBJD's display date ODDDAT (the model's
/// field description; PUB400 2026-10-26, verify2 o_objd_vals: 261026 in
/// a *YMD job).
pub fn job_date(yymmdd: &str) -> String {
    if yymmdd.len() != 6 {
        return yymmdd.to_string();
    }
    let (y, m, d) = (&yymmdd[0..2], &yymmdd[2..4], &yymmdd[4..6]);
    let f = crate::clpgm::system_value_text("QDATFMT")
        .and_then(|v| v.ok())
        .unwrap_or_default();
    match f.as_str() {
        "YMD" => format!("{y}{m}{d}"),
        "DMY" => format!("{d}{m}{y}"),
        _ => format!("{m}{d}{y}"),
    }
}
