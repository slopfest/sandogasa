Name:           python-cstq
Version:        0.0.20
Release:        %autorelease
# Fill in the actual package summary to submit package to Fedora
Summary:        ...

# Check if the automatically generated License and its spelling is correct for Fedora
# https://docs.fedoraproject.org/en-US/packaging-guidelines/LicensingGuidelines/
License:        MIT
URL:            https://github.com/aleivag/cstq
Source:         %{pypi_source cstq}

BuildArch:      noarch
BuildRequires:  python3-devel


# Fill in the actual package description to submit package to Fedora
%global _description %{expand:
This is package 'cstq' generated automatically by pyp2spec.}

%description %_description

%package -n     python3-cstq
Summary:        %{summary}

%description -n python3-cstq %_description


%prep
%autosetup -p1 -n cstq-%{version}


%generate_buildrequires
%pyproject_buildrequires


%build
%pyproject_wheel


%install
%pyproject_install
# Automatically extracted from wheel
%pyproject_save_files -l cstq


%check
%pyproject_check_import


%files -n python3-cstq -f %{pyproject_files}


%changelog
%autochangelog
