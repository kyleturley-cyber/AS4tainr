      * CUSTLIST: list customers in key order, then look one up by key.
      * Exercises READ, CHAIN, %EOF and %FOUND against CUSTMAST.
      * Parameter: customer number to look up (5,0).
     FCUSTMAST  IF   E           K DISK
     D msg             S             52
     D toFind          S              5  0
     C     *entry        plist
     C                   parm                    toFind
      * Full pass in key order
     C                   read      CUSTMAST
     C                   dow       not %eof(CUSTMAST)
     C                   eval      msg = %char(CUSNO) + ' ' + %trim(CUSNAM)
     C     msg           dsply
     C                   read      CUSTMAST
     C                   enddo
      * Random access by key
     C     toFind        chain     CUSTMAST
      * %FOUND without a file name (last file operation): Jariko v2.2.0
      * does not implement %FOUND(file) yet. See docs/fidelity-gaps.md.
     C                   if        %found
     C                   eval      msg = 'Found ' + %trim(CUSNAM) + ' in '
     C                                   + %trim(CUSCTY)
     C                   else
     C                   eval      msg = 'Customer not found'
     C                   endif
     C     msg           dsply
     C                   seton                                        lr
