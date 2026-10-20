     DPOS              S              3S 0
     DPOSA             S              3S 0 DIM(3)
     DRES              S              5
     DR10              S             10
     DF5               S              5    INZ('ABC')
     DARR              S              2    DIM(5) CTDATA PERRCD(5)
     DTABA             S              1    DIM(3) CTDATA PERRCD(3)
     DTABB             S              1    DIM(3) ALT(TABA)
     DI                S              3S 0
     DNUM              S              5P 2 DIM(3)
     DTOT              S              7P 2
     DSQ               S              5P 2
     DCH               S              2    DIM(3)
     DOCC              DS                  OCCURS(3)
     DOFLD                            4
     DB1               S              1    INZ(X'00')
     DT3               S              3
     C     'ABC'         SCAN      'XXABCXXABC'  POS                      50
     C     POS           DSPLY
     C     'ABC'         SCAN      'XXABCXXABC'  POSA
     C     POSA(1)       DSPLY
     C     POSA(2)       DSPLY
     C     POSA(3)       DSPLY
     C     'ABC'         SCAN      'XXABCXXABC':4POS
     C     POS           DSPLY
     C     'ABD'         SCAN      'XXABCXXABC'  POS                      50
     C     POS           DSPLY
     C  N50'NOTFOUND'    DSPLY
     C     '0123456789'  CHECK     '12A45'       POS
     C     POS           DSPLY
     C     ' '           CHECKR    'AB  '        POS
     C     POS           DSPLY
     C                   EVAL      RES = 'XXXXX'
     C     3             SUBST     'ABCDEFG':2   RES
     C     RES           DSPLY
     C     3             SUBST(P)  'ABCDEFG':2   RES
     C     RES           DSPLY
     C     'abc':'ABC'   XLATE     'aabbxc'      R10
     C     R10           DSPLY
     C     'abc':'ABC'   XLATE(P)  'aabbxc':3    R10
     C     R10           DSPLY
     C     F5            CAT(P)    'DEF':1       R10
     C     R10           DSPLY
     C     F5            CAT(P)    'DEF'         R10
     C     R10           DSPLY
     C                   Z-ADD     1             I
     C     'CC'          LOOKUP    ARR(I)                                 51
     C     I             DSPLY
     C   51'FOUND CC'    DSPLY
     C                   Z-ADD     1             I
     C     'ZZ'          LOOKUP    ARR(I)                                 51
     C     I             DSPLY
     C  N51'NO ZZ'       DSPLY
     C     'B'           LOOKUP    TABA          TABB                     52
     C     TABB          DSPLY
     C                   Z-ADD     1.5           NUM(1)
     C                   Z-ADD     2.25          NUM(2)
     C                   Z-ADD     3             NUM(3)
     C                   XFOOT     NUM           TOT
     C     TOT           DSPLY
     C                   SQRT(H)   10            SQ
     C     SQ            DSPLY
     C                   SQRT      2             SQ
     C     SQ            DSPLY
     C                   MOVEA     'EFCDAB'      CH
     C     CH(1)         DSPLY
     C     CH(3)         DSPLY
     C                   SORTA     CH
     C     CH(1)         DSPLY
     C     CH(3)         DSPLY
     C                   MOVEA     'XY'          CH(2)
     C     CH(2)         DSPLY
     C     2             OCCUR     OCC
     C                   MOVE      'TWO '        OFLD
     C     1             OCCUR     OCC
     C                   MOVE      'ONE '        OFLD
     C     2             OCCUR     OCC
     C     OFLD          DSPLY
     C                   OCCUR     OCC           I
     C     I             DSPLY
     C                   MOVE      '123'         T3
     C                   TESTN                   T3                   535455
     C   53'NUMERIC'     DSPLY
     C                   MOVE      ' 12'         T3
     C                   TESTN                   T3                   535455
     C   54'LEADING'     DSPLY
     C                   BITON     '01'          B1
     C                   TESTB     '01'          B1                   565758
     C   58'BITS ON'     DSPLY
     C                   TESTB     '012'         B1                   565758
     C   57'BITS MIXED'  DSPLY
     C     *LIKE         DEFINE    TOT           TOT2             +2
     C                   Z-ADD     123456.78     TOT2
     C     TOT2          DSPLY
     C                   SETON                                        60
     C                   SETOFF                                       61
     C   60
     CAN 61'AND NO'      DSPLY
     C   61
     COR 60'OR YES'      DSPLY
     C                   Z-ADD     2             I
     C     I             CASEQ     1             SR1
     C     I             CASEQ     2             SR2
     C                   CAS                     SR3
     C                   ENDCS
     C                   SETON                                        LR
     C     SR1           BEGSR
     C     'SR1'         DSPLY
     C                   ENDSR
     C     SR2           BEGSR
     C     'SR2'         DSPLY
     C                   ENDSR
     C     SR3           BEGSR
     C     'SR3'         DSPLY
     C                   ENDSR
**CTDATA ARR
AABBCCDDEE
**CTDATA TABA
A1B2C3
