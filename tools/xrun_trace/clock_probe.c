#define _GNU_SOURCE
#include <sched.h>
#include <time.h>
#include <unistd.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
static uint64_t now(void) { struct timespec t; if(clock_gettime(CLOCK_MONOTONIC,&t)) exit(1); return (uint64_t)t.tv_sec*1000000000+t.tv_nsec; }
int main(void) {
 int fd=open("/sys/kernel/debug/tracing/trace_marker",O_WRONLY); if(fd<0)return 1;
 for(int cpu=0;cpu<4;cpu++) {
  cpu_set_t set;CPU_ZERO(&set);CPU_SET(cpu,&set);if(sched_setaffinity(0,sizeof(set),&set))return 2;
  for(int n=0;n<8;n++) {
   char marker[80];int len=snprintf(marker,sizeof(marker),"RC2FX_CLOCK cpu=%d n=%d\n",cpu,n);
   uint64_t before=now();if(write(fd,marker,len)!=len)return 3;uint64_t after=now();
   printf("%d %d %llu %llu\n",cpu,n,(unsigned long long)before,(unsigned long long)after);
  }
 }
 return close(fd)!=0;
}
