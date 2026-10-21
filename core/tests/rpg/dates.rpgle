     H DATFMT(*USA)
      * Date and time formats (ILE RPG Reference, "Date Data Type", "Time
      * Data Type", DATFMT, TIMFMT, "Moving Date-Time Data").
     D d               S               D   INZ(D'2026-10-21')
     D m               S               D   DATFMT(*MDY)
     D j               S               D   DATFMT(*JUL)
     D e               S               D   DATFMT(*EUR)
     D t               S               T   INZ(T'13.45.00') TIMFMT(*HMS)
     D z               S               Z   INZ(Z'2026-01-02-03.04.05.000006')
     D c               S             10A   INZ('XXXXXXXXXX')
     D n               S              8  0
     D n6              S              6  0
     D ds              DS
     D  dsm                            D   DATFMT(*MDY)
     D  dsr                           2A
      * The control specification's format, a field's own.
     C                   EVAL      m = d
     C                   EVAL      j = d
     C                   EVAL      e = d
     C     d             DSPLY
     C     m             DSPLY
     C                   EVAL      c = %CHAR(j)
     C     c             DSPLY
     C                   EVAL      c = %CHAR(e)
     C     c             DSPLY
     C     t             DSPLY
      * A date subfield is stored in its format: 8 bytes.
     C                   EVAL      dsm = d
     C                   EVAL      dsr = 'ZZ'
     C     ds            DSPLY
      * MOVE: factor 1 is the other side's format.
     C     *MDY          MOVE      d             c
     C     c             DSPLY
     C     *USA          MOVE      d             n
     C     n             DSPLY
     C     *MDY          MOVE      '12/25/39'    d
     C     *ISO          MOVEL     d             c
     C     c             DSPLY
     C     *YMD          MOVE      260102        d
     C                   MOVE      d             z
     C     *ISO          MOVEL     z             c
     C     c             DSPLY
     C     *HMS          MOVE      t             n6
     C     n6            DSPLY
     C                   EVAL      c = %CHAR(d + %DAYS(30))
     C     c             DSPLY
     C                   EVAL      n = %DEC(m)
     C     n             DSPLY
     C                   EVAL      d = %DATE('01/02/2027')
     C     d             DSPLY
     C                   SETON                                        LR
