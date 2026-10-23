/* ugrep 7.8.4 的 config.h，目标族 linux-gnu（docs/specs/rust-cli/embedded-search.md §7.2）。
   由 scripts/rust-cli/vendor/ugrep-config.mjs 用 ugrep 的 configure 生成，不手改；库由 build.rs 编，这里打开。 */
/* config.h.  Generated from config.h.in by configure.  */
/* config.h.in.  Generated from configure.ac by autoheader.  */

/* Signal to boost lib that it is being used without the rest of boost */
#define BOOST_REGEX_STANDALONE 1

/* Define if built with 7ZIP support */
#define HAVE_7ZIP 1

/* define if the Boost::Regex library is available */
/* #undef HAVE_BOOST_REGEX */

/* Define to 1 if you have the 'cpuset_setaffinity' function. */
/* #undef HAVE_CPUSET_SETAFFINITY */

/* define if the compiler supports basic C++11 syntax */
#define HAVE_CXX11 1

/* Define to 1 if you have the <dirent.h> header file, and it defines 'DIR'.
   */
#define HAVE_DIRENT_H 1

/* Define if F_RDAHEAD fcntl() is supported */
/* #undef HAVE_F_RDAHEAD */

/* Define to 1 if you have the 'getpagesize' function. */
#define HAVE_GETPAGESIZE 1

/* Define to 1 if you have the <inttypes.h> header file. */
#define HAVE_INTTYPES_H 1

/* Define to 1 if you have `brotli' library (-lbrotlidec -lbrotlienc) */
#define HAVE_LIBBROTLI 1

/* Define to 1 if you have `bz2' library (-lbz2) */
#define HAVE_LIBBZ2 1

/* Define to 1 if you have `bzip3' library (-lbzip3) */
/* #undef HAVE_LIBBZIP3 */

/* Define to 1 if you have `lz4' library (-llz4) */
/* #undef HAVE_LIBLZ4 */

/* Define to 1 if you have `lzma' library (-llzma) */
/* #undef HAVE_LIBLZMA */

/* Define to 1 if you have the 'ws2_32' library (-lws2_32). */
/* #undef HAVE_LIBWS2_32 */

/* Define to 1 if you have `z' library (-lz) */
#define HAVE_LIBZ 1

/* Define to 1 if you have `zstd' library (-lzstd) */
#define HAVE_LIBZSTD 1

/* Define to 1 if you have a working 'mmap' system call. */
#define HAVE_MMAP 1

/* Define to 1 if you have the <ndir.h> header file, and it defines 'DIR'. */
/* #undef HAVE_NDIR_H */

/* Define if O_NOATIME open flag is supported */
#define HAVE_O_NOATIME 1

/* Define to 1 if you have `PCRE2' library (-lpcre2-8) */
#define HAVE_PCRE2 1

/* Define if you have POSIX threads libraries and header files. */
#define HAVE_PTHREAD 1

/* Have PTHREAD_PRIO_INHERIT. */
#define HAVE_PTHREAD_PRIO_INHERIT 1

/* Define to 1 if you have the 'pthread_setaffinity_np' function. */
/* #undef HAVE_PTHREAD_SETAFFINITY_NP */

/* Define to 1 if you have the 'pthread_setschedprio' function. */
/* #undef HAVE_PTHREAD_SETSCHEDPRIO */

/* Define to 1 if you have the 'pthread_set_qos_class_self_np' function. */
/* #undef HAVE_PTHREAD_SET_QOS_CLASS_SELF_NP */

/* Define to 1 if you have the <sched.h> header file. */
#define HAVE_SCHED_H 1

/* Define to 1 if you have the 'sched_setaffinity' function. */
#define HAVE_SCHED_SETAFFINITY 1

/* Define to 1 if you have the 'setpriority' function. */
/* #undef HAVE_SETPRIORITY */

/* Define to 1 if you have the 'statfs' function. */
#define HAVE_STATFS 1

/* Define to 1 if you have the 'statvfs' function. */
#define HAVE_STATVFS 1

/* Define to 1 if you have the <stdint.h> header file. */
#define HAVE_STDINT_H 1

/* Define to 1 if you have the <stdio.h> header file. */
#define HAVE_STDIO_H 1

/* Define to 1 if you have the <stdlib.h> header file. */
#define HAVE_STDLIB_H 1

/* Define to 1 if you have the <strings.h> header file. */
#define HAVE_STRINGS_H 1

/* Define to 1 if you have the <string.h> header file. */
#define HAVE_STRING_H 1

/* Define to 1 if you have the 'strncasecmp' function. */
#define HAVE_STRNCASECMP 1

/* Define to 1 if you have the 'strnicmp' function. */
/* #undef HAVE_STRNICMP */

/* Define to 1 if 'd_ino' is a member of 'struct dirent'. */
#define HAVE_STRUCT_DIRENT_D_INO 1

/* Define to 1 if 'd_type' is a member of 'struct dirent'. */
#define HAVE_STRUCT_DIRENT_D_TYPE 1

/* Define to 1 if 'st_atim' is a member of 'struct stat'. */
#define HAVE_STRUCT_STAT_ST_ATIM 1

/* Define to 1 if 'st_atimespec' is a member of 'struct stat'. */
/* #undef HAVE_STRUCT_STAT_ST_ATIMESPEC */

/* Define to 1 if 'st_ctim' is a member of 'struct stat'. */
#define HAVE_STRUCT_STAT_ST_CTIM 1

/* Define to 1 if 'st_ctimespec' is a member of 'struct stat'. */
/* #undef HAVE_STRUCT_STAT_ST_CTIMESPEC */

/* Define to 1 if 'st_mtim' is a member of 'struct stat'. */
#define HAVE_STRUCT_STAT_ST_MTIM 1

/* Define to 1 if 'st_mtimespec' is a member of 'struct stat'. */
/* #undef HAVE_STRUCT_STAT_ST_MTIMESPEC */

/* Define to 1 if you have the <sys/cpuset.h> header file. */
/* #undef HAVE_SYS_CPUSET_H */

/* Define to 1 if you have the <sys/dir.h> header file, and it defines 'DIR'.
   */
/* #undef HAVE_SYS_DIR_H */

/* Define to 1 if you have the <sys/mount.h> header file. */
#define HAVE_SYS_MOUNT_H 1

/* Define to 1 if you have the <sys/ndir.h> header file, and it defines 'DIR'.
   */
/* #undef HAVE_SYS_NDIR_H */

/* Define to 1 if you have the <sys/param.h> header file. */
#define HAVE_SYS_PARAM_H 1

/* Define to 1 if you have the <sys/resource.h> header file. */
#define HAVE_SYS_RESOURCE_H 1

/* Define to 1 if you have the <sys/statvfs.h> header file. */
#define HAVE_SYS_STATVFS_H 1

/* Define to 1 if you have the <sys/stat.h> header file. */
#define HAVE_SYS_STAT_H 1

/* Define to 1 if you have the <sys/time.h> header file. */
#define HAVE_SYS_TIME_H 1

/* Define to 1 if you have the <sys/types.h> header file. */
#define HAVE_SYS_TYPES_H 1

/* Define to 1 if you have the <unistd.h> header file. */
#define HAVE_UNISTD_H 1

/* Define to 1 if you have the '_strnicmp' function. */
/* #undef HAVE__STRNICMP */

/* Name of package */
#define PACKAGE "ugrep"

/* Define to the address where bug reports for this package should be sent. */
#define PACKAGE_BUGREPORT "https://github.com/Genivia/ugrep/issues"

/* Define to the full name of this package. */
#define PACKAGE_NAME "ugrep"

/* Define to the full name and version of this package. */
#define PACKAGE_STRING "ugrep 7.8.4"

/* Define to the one symbol short name of this package. */
#define PACKAGE_TARNAME "ugrep"

/* Define to the home page for this package. */
#define PACKAGE_URL "https://ugrep.com"

/* Define to the version of this package. */
#define PACKAGE_VERSION "7.8.4"

/* Define to necessary symbol if this constant uses a non-standard name on
   your system. */
/* #undef PTHREAD_CREATE_JOINABLE */

/* Define to 1 if all of the C89 standard headers exist (not just the ones
   required in a freestanding environment). This macro is provided for
   backward compatibility; new code need not use it. */
#define STDC_HEADERS 1

/* Version number of package */
#define VERSION "7.8.4"

/* Define as 'unsigned int' if <stddef.h> doesn't define. */
/* #undef size_t */

/* Define as 'int' if <sys/types.h> doesn't define. */
/* #undef ssize_t */
