/* Copyright 2011-2012 Nicholas J. Kain, licensed under standard MIT license */
/* Nova OS: the TLS base is set by SYS_THREAD_SET_TLS(ARCH_SET_FS, base). */
.text
.global __set_thread_area
.hidden __set_thread_area
.type __set_thread_area,@function
__set_thread_area:
	mov %rdi,%rsi           /* shift for syscall */
	movl $0x1002,%edi       /* SET_FS register */
	movl $0x0A04,%eax       /* SYS_THREAD_SET_TLS */
	syscall
	ret
