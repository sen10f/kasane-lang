#include <sys/socket.h>
#include <stdio.h>
#include <string.h>
#include <stdint.h>
typedef long long i64; typedef unsigned long long u64; typedef unsigned char u8; typedef int i32;
i64 ksn_rt_sys_net_recvfrom(i32 fd, u8 *buf, u64 n, u8 *from, u64 fcap, i32 *e);
int main(void){ int sv[2]; socketpair(AF_UNIX,SOCK_DGRAM,0,sv); send(sv[0],"hi",2,0);
 u8 b[8]; u8 from[100]; memset(from,'X',100); i32 e=0; i64 r=ksn_rt_sys_net_recvfrom(sv[1],b,8,from,100,&e);
 printf("%lld e=%d from=[%s]\n",r,e,from); return 0;}
int ksn_rt_argc; char **ksn_rt_argv;
