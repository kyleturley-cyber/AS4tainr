**FREE
// verify29: a module for a binder source with two *CURRENT blocks
ctl-opt nomain;
dcl-proc gld29one export;
  dcl-pi *n int(10) end-pi;
  return 1;
end-proc;
dcl-proc gld29two export;
  dcl-pi *n int(10) end-pi;
  return 2;
end-proc;
