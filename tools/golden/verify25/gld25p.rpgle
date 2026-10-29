**FREE
// verify25: a procedure exported by its interface's EXTPROC name
ctl-opt nomain;
dcl-proc mockp1 export;
  dcl-pi *n packed(7:2) extproc('Gld25_Price');
    item char(5) const;
  end-pi;
  return 12.5;
end-proc;
dcl-proc gld25cnt export;
  dcl-pi *n int(10) end-pi;
  return 3;
end-proc;
