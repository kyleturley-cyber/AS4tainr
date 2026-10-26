             CMD        PROMPT('Golden test') VLDCKR(GLDVCHK) +
                          ALLOW(*BPGM *IPGM) TEXT(*CMDPMT)
             PARM       KWD(IN) TYPE(*CHAR) LEN(10) MIN(1)
             PARM       KWD(OUT) TYPE(*CHAR) LEN(10) RTNVAL(*YES) MIN(1)
