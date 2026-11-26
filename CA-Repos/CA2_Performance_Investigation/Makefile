.PHONY: all test quick clean

all:
	python run_all.py

quick:
	python run_all.py --quick

test:
	python -m unittest discover -s tests -v

clean:
	rm -rf results/*.csv results/*.json results/*.txt results/tables results/figures
