**FREE
// verify34: %STATUS and the PSDS after MONITOR caught a divide by zero
// in a service program's procedure: read at once in ON-ERROR, after a
// call of another procedure of that service program that ends
// normally, after CALLP(E), and for a procedure found with QleActBndPgm and QleGetExp
// (as a test runner calls test procedures). One line each into
// AS4TRAINR1/GLD34OUT. Created with CRTPGM ACTGRP(*NEW).
ctl-opt option(*srcstmt);
dcl-ds psds psds qualified;
  status zoned(5) pos(11);
  exceptionId char(7) pos(40);
end-ds;
dcl-pr divz int(10) extproc('DIVZ');
  d int(10) value;
end-pr;
dcl-pr okay ind extproc('OKAY');
end-pr;
dcl-pr byPtr extproc(ptr);
  d int(10) value;
end-pr;
dcl-pr rslvsp pointer(*proc) extproc('rslvsp');
  objectType char(2) value;
  objectName pointer value options(*string);
  objectLib pointer value options(*string);
  authority char(2) value;
end-pr;
dcl-pr QleActBndPgm extproc('QleActBndPgm');
  boundProgram pointer(*proc) const;
  activationMark int(10) options(*omit);
  activationInfo char(64) options(*omit);
  activationInfoLength int(10) const options(*omit);
  errorCode char(32767) options(*varsize: *omit);
end-pr;
dcl-pr QleGetExp pointer(*proc) extproc('QleGetExp');
  activationMark int(10) const;
  exportNumber int(10) const;
  exportNameLength int(10) const;
  exportName char(256) const options(*varsize);
  exportPointer pointer(*proc) options(*omit);
  exportType int(10);
  errorCode char(32767) options(*varsize: *omit);
end-pr;
dcl-ds err qualified;
  provided int(10) inz(16);
  available int(10) inz(0);
  msgid char(7);
  reserved char(1);
end-ds;
dcl-s ptr pointer(*proc);
dcl-s srv pointer(*proc);
dcl-s mark int(10);
dcl-s kind int(10);
dcl-s name char(256);
dcl-s r int(10);
dcl-s n int(10) inz(0);
dcl-s flag ind;
dcl-s st int(10);

monitor;
  r = divz(0);
on-error;
  st = %status();
  note('at once': st);
endmon;
monitor;
  r = divz(0);
on-error;
  flag = okay();
  note('after a call': %status());
endmon;
callp(e) divz(0);
note('callp(e)': %status());
srv = rslvsp(x'0203': 'GLD34V': 'AS4TRAINR1': x'0000');
QleActBndPgm(srv: mark: *omit: *omit: *omit);
name = 'DIVZ';
QleGetExp(mark: 0: 4: name: ptr: kind: err);
monitor;
  byPtr(0);
on-error;
  flag = okay();
  note('QleGetExp': %status());
endmon;
*inlr = *on;

dcl-proc note;
  dcl-pi *n;
    what varchar(30) const;
    status int(10) const;
  end-pi;
  dcl-s line varchar(200);
  n += 1;
  line = what + ' status=' + %char(status) + ' psds=' + %char(psds.status)
       + ' id=' + psds.exceptionId;
  exec sql INSERT INTO AS4TRAINR1.GLD34OUT VALUES (:n, :line);
end-proc;
