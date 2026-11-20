/* Windows クロスビルド用スタブ。
   組み込み ldd をリンクできない (Windows LLVM インストールに
   liblld*.a が含まれない) ため、ksn_lld_link と Windows API の
   GUID 定数をスタブで提供する。
   ksn_lld_link: lld_shim.cpp で定義されるが、Windows クロスでは
   組み込み ldd をスキップするためスタブが必要。
   GUID 定数: libuuid.a に含まれるはずだが、リンク順序の問題で
   未解決になることがあるためスタブで提供する。
   GUID 値は実際の Windows SDK の値。ksn build --release で
   マニフェスト統合は行わないため、値は参照されないだけ。
*/
typedef struct _GUID {
    unsigned long Data1;
    unsigned short Data2;
    unsigned short Data3;
    unsigned char Data4[8];
} GUID;

extern const GUID CLSID_FileOperation =
    {0x33032834, 0xF597, 0x44BC, {0x95, 0x36, 0x75, 0x84, 0x62, 0x99, 0xE4, 0xE4}};
extern const GUID FOLDERID_LocalAppData =
    {0xF1B32785, 0x6FBA, 0x4FCF, {0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91}};
extern const GUID FOLDERID_Profile =
    {0x5E6C858F, 0x0E22, 0x4760, {0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73}};

extern const GUID IID_IFileOperation =
    {0x971C7C9C, 0x824A, 0x4EAC, {0x87, 0x97, 0xC9, 0x13, 0x51, 0x2F, 0x39, 0x01}};
extern const GUID IID_IFileOperationProgressSink =
    {0x967A5B51, 0x5D5C, 0x4A80, {0xA5, 0xD3, 0x9C, 0x1A, 0x5D, 0x2E, 0x5F, 0x8A}};

int ksn_lld_link(int flavor, int argc, const char **argv, char **out, char **err) {
    (void)flavor;
    (void)argc;
    (void)argv;
    (void)out;
    (void)err;
    if (out) *out = 0;
    if (err) *err = 0;
    return 1; /* エラー: Windows クロスでは組み込み ldd は未サポート */
}
