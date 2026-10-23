     H DFTACTGRP(*NO) ACTGRP(*NEW)
      * Golden spooled file for as4tainr (tools/golden/spool.txt): lines
      * printed with space before, space after and skip before, as RPGUnit
      * prints its report (OVRPRTF USRDTA SPLFNAME, EXCEPT, space before 1).
     FQSYSPRT   O    F   80        PRINTER OFLIND(*INOF) USROPN
     D text            S             80A
     D cmd             S            200A
     D QCMDEXC         PR                  EXTPGM('QCMDEXC')
     D  c                           200A   CONST
     D  l                            15P 5 CONST
      /FREE
        cmd = 'OVRPRTF FILE(QSYSPRT) USRDTA(GLDSPL) SPLFNAME(GLDSPL)';
        QCMDEXC(cmd : 200);
        open QSYSPRT;
        text = 'LINE ONE';
        except SB1;
        text = '  INDENTED';
        except SB1;
        text = 'A    B    C';
        except SB1;
        text = 'AFTER A BLANK LINE';
        except SB2;
        text = 'SKIPPED TO LINE 10';
        except SK10;
        text = 'SPACE AFTER 3';
        except SA3;
        text = 'PAGE TWO';
        except SK1;
        text = 'LAST';
        except SB1;
        close QSYSPRT;
        cmd = 'DLTOVR FILE(QSYSPRT)';
        QCMDEXC(cmd : 200);
        *inlr = *on;
      /END-FREE
     OQSYSPRT   E            SB1         1
     O                       text                80
     O          E            SB2         2
     O                       text                80
     O          E            SK10             10
     O                       text                80
     O          E            SA3            3
     O                       text                80
     O          E            SK1               1
     O                       text                80
