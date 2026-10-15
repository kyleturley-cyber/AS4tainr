      * ORDTOTAL: total one order with SETLL + READE on a partial key.
      * Parameter: order number (7,0).
     FORDLIN    IF   E           K DISK
     D msg             S             52
     D ordToFind       S              7  0
     D total           S             11  2
     D lines           S              5  0
     C     *entry        plist
     C                   parm                    ordToFind
     C     ordToFind     setll     ORDLIN
     C     ordToFind     reade     ORDLIN
     C                   dow       not %eof(ORDLIN)
     C                   eval      total = total + QTY * PRICE
     C                   eval      lines = lines + 1
     C     ordToFind     reade     ORDLIN
     C                   enddo
     C                   eval      msg = 'Order ' + %char(ordToFind) + ': '
     C                                   + %char(lines) + ' lines, total '
     C                                   + %char(total)
     C     msg           dsply
     C                   seton                                        lr
