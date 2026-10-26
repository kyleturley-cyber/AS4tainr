**free
// Golden: the call stack as QWVRCSTK lists it (program names, most
// recent first, this service program left out) and the information
// status after a call whose receiver is only the header.
ctl-opt nomain;
dcl-proc Callers export;
  dcl-pi *n;
    res char(60);
  end-pi;
  dcl-pr getStack extpgm('QWVRCSTK');
    rcv char(65535) options(*varsize);
    len int(10) const;
    fmt char(8) const;
    jid char(56) const;
    jfmt char(8) const;
    err char(16);
  end-pr;
  dcl-ds hdr qualified;
    rtn int(10);
    avl int(10);
    ent int(10);
    off int(10);
    nret int(10);
    thread char(8);
    status char(1);
  end-ds;
  dcl-s buf char(8192);
  dcl-s jid char(56);
  dcl-s err char(16) inz(x'00000010');
  dcl-s i int(10);
  dcl-s p pointer;
  dcl-ds e qualified based(p);
    len int(10) pos(1);
    pgm char(10) pos(25);
  end-ds;
  // JIDF0100: job name '*', user and number blank, internal job ID
  // blank, 2 reserved bytes x'00', thread indicator 1 (this thread).
  jid = '*';
  %subst(jid:43:2) = x'0000';
  %subst(jid:45:4) = x'00000001';
  %subst(jid:49:8) = *allx'00';
  getStack(hdr:%size(hdr):'CSTK0100':jid:'JIDF0100':err);
  res = 'S' + hdr.status + '|';
  if %subst(err:5:4) <> x'00000000';
    res = 'ERR ' + %subst(err:9:7);
    return;
  endif;
  getStack(buf:%size(buf):'CSTK0100':jid:'JIDF0100':err);
  hdr = buf;
  if %subst(err:5:4) <> x'00000000' or hdr.nret > 50;
    res = 'ERR ' + %subst(err:9:7);
    return;
  endif;
  p = %addr(buf) + hdr.off;
  for i = 1 to hdr.nret;
    if e.pgm <> 'GLDVSTK';
      res = %trimr(res) + %trimr(e.pgm) + '/';
    endif;
    p += e.len;
  endfor;
end-proc;
