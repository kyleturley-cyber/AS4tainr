# Menus

After sign-on, as4tainr shows the user's **initial menu**, as IBM i does. For a new profile, and for QSECOFR, that is the main menu, MAIN:

```
 MAIN                          as4tainr Main Menu       System  . .:   AS4TAINR
                                                      User  . . .:   QSECOFR
 Select one of the following:

      1. User tasks
      2. Jobs and printed output
      3. Files, libraries and objects
      4. Programming
      5. Security
      6. Display a menu
      7. Command entry

     90. Sign off





 Selection or command
 ===>

 F3=Exit   F4=Prompt   F9=Retrieve   F12=Cancel
```

## Using a menu

- Type an option number and press Enter. Each option runs a command. Most open another menu (`GO USER`) or run a command. Some prompt a command first, such as option 2 on USER, which prompts SNDMSG.
- Any command can be typed on the **Selection or command** line. It runs there, and its messages show on the message line. Displays the command shows (lists, programs, STRSQL) come back to the menu when they end.
- **F4** prompts the command typed. On an empty line, F4 shows the command groups.
- **F9** retrieves earlier commands.
- **F12** goes back one menu.
- **F3** leaves the menus, back to Command Entry if you came from there. On the initial menu, F3 signs off.
- **Option 90** signs off from any system menu.
- **Command Entry** (option 7, or `CALL QCMD`) is the command line with a log. F3 there goes back to the menu.

A `?` in front of a command name prompts it. For example, `?CRTLIB` shows the CRTLIB prompt. This works on any command line.

## The system menus

These menus are built into as4tainr and belong to QSYS. Their texts are as4tainr's own. They lead to the commands as4tainr has, so they are not copies of the IBM i menus.

| Menu | Title | Options |
| --- | --- | --- |
| MAIN | as4tainr Main Menu | the menus below; 6 prompts GO; 7 is Command Entry |
| USER | User Tasks | DSPMSG, SNDMSG, WRKSPLF, WRKSBMJOB, DSPJOBLOG, CHGPWD, DSPLIBL |
| JOBS | Jobs and Printed Output | WRKACTJOB, WRKSBMJOB, SBMJOB, ENDJOB, WRKSPLF, DSPJOBLOG |
| FILES | Files, Libraries and Objects | DSPLIBL, DSPLIB, CRTLIB, WRKOBJ, DSPPFM, DSPDTAARA, STRSQL |
| PROGRAM | Programming | Command Entry, CALL, STRSQL, CRTMNU, CRTDTAARA |
| SECURITY | Security | CRTUSRPRF, CHGUSRPRF, DSPUSRPRF, DLTUSRPRF, DSPOBJAUT, GRTOBJAUT, RVKOBJAUT, CHGOBJOWN |

Every system menu has option 90, Sign off. `GO MAIN` (or any of the names above) shows a menu from anywhere.

## Initial program and initial menu

The user profile decides what happens after sign-on. These are the IBM i parameters of CRTUSRPRF and CHGUSRPRF:

| Parameter | Default | Meaning |
| --- | --- | --- |
| `INLPGM` | `*NONE` | A program called right after sign-on, before the menu. It is often a CL program that sets up the library list. |
| `INLMNU` | `MAIN` | The menu shown after the initial program ends. With `*SIGNOFF`, the user is signed off when the initial program ends, so the user only ever sees that program. |

```
CRTUSRPRF USRPRF(CLERK) PASSWORD(...) INLPGM(QGPL/ORDENTC) INLMNU(*SIGNOFF)
CHGUSRPRF USRPRF(CLERK) INLPGM(*NONE) INLMNU(SECURITY)
```

DSPUSRPRF shows both values. If the initial menu cannot be found, the session opens on Command Entry and says why.

## Your own menus: CRTMNU TYPE(*PGM)

A menu of type `*PGM` is a program. GO calls it, and the program shows the menu with its own display file. When the program returns, the menu is done, and the display you came from comes back.

```
CRTMNU MENU(MYLIB/ORDERS) TYPE(*PGM) PGM(QGPL/ORDENTC) TEXT('Order entry')
GO MYLIB/ORDERS
DLTMNU MYLIB/ORDERS
```

- `PGM(*MENU)` means a program with the menu's name, in the menu's library.
- The menu is a `*MENU` object owned by its creator. GO needs *USE authority to it, and DLTMNU needs *ALL.
- The initial menu can be a `*PGM` menu too.

Menus of type `*DSPF` (a display file whose options come from a message file) and `*UIM` (panel groups) are not supported yet. See [fidelity-gaps.md](fidelity-gaps.md).
