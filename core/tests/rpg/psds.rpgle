**FREE
// The program status and file information data structures (ILE RPG
// Reference, "Program Status Data Structure", "File Information Data
// Structure") and %STATUS after operations ("File Status Codes",
// "Program Status Codes": 00011 end of file, 00012 no record found,
// 01021 duplicate key, 00102 divide by zero).
dcl-f custmast usage(*update:*output) keyed infds(custfb);
dcl-ds pgmstat psds qualified;
  proc *proc;
  status *status;
  prvstatus zoned(5:0) pos(16);
  routine *routine;
  parms *parms;
  excpid char(7) pos(40);
  user char(10) pos(358);
end-ds;
dcl-ds custfb;
  fbfile *file;
  fbopen char(1) pos(9);
  fbeof char(1) pos(10);
  fbstatus *status;
  fbopcode *opcode;
  fbrecord *record;
  fbrrn int(10) pos(397);
  fbreclen int(10) pos(283);
  fbreads int(10) pos(247);
end-ds;
dcl-s n packed(5:0);
dcl-s z packed(5:0);
dsply pgmstat.proc;
dsply %char(pgmstat.parms);
dsply pgmstat.user;
chain 9 custmast;
dsply %char(%status(custmast)) + ' ' + %char(%status);
dsply %subst(fbopcode:1:5) + ' ' + fbrecord + ' ' + fbopen + fbfile;
chain 2 custmast;
dsply %char(fbstatus) + ' ' + %char(fbrrn) + ' ' + %char(fbreclen);
setll *hival custmast;
read custmast;
dsply %char(%status) + ' ' + fbeof + ' ' + %char(fbreads);
cusno = 1;
cusnam = 'Duplicate';
write(e) custrec;
dsply %char(%error) + ' ' + %char(%status) + ' ' + %char(fbstatus);
monitor;
  n = 1 / z;
on-error 102;
  dsply 'divide ' + %char(pgmstat.status) + ' ' + %char(pgmstat.prvstatus)
        + ' ' + pgmstat.excpid;
endmon;
*inlr = *on;
