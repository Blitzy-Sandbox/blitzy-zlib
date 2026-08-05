all:
	-@echo "Please use ./configure first.  Thank you."

distclean:
	make -f Makefile.in distclean

rust:
	make -f Makefile.in rust

rust-test:
	make -f Makefile.in rust-test

rust-clean:
	make -f Makefile.in rust-clean
